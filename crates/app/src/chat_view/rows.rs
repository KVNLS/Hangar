//! The rows of the virtualized transcript list: which items the current view shows, and the
//! list operations that bring the list up to date after items changed.

use std::ops::Range;

/// A row of the transcript list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    /// The prompt a subagent was given (its view only).
    Task,
    /// `ChatView::items[i]`.
    Item(usize),
    /// What a subagent returned (its view only).
    Result,
    /// "Ready for review" (the agent's view only).
    Done,
    /// The "Working…" line.
    Working,
}

/// A change to apply to the list state, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ListOp {
    /// The rows in the range were replaced by this many new rows.
    Splice(Range<usize>, usize),
    /// The rows in the range changed in place and must be measured again.
    Remeasure(Range<usize>),
}

/// The rows the list holds: `lead`, the visible items, then `trail`.
#[derive(Debug, Default)]
pub(super) struct Rows {
    lead: Vec<Row>,
    /// Indexes of the visible items, ascending.
    items: Vec<usize>,
    trail: Vec<Row>,
    /// Items below this index have been seen.
    synced: usize,
}

impl Rows {
    pub fn len(&self) -> usize {
        self.lead.len() + self.items.len() + self.trail.len()
    }

    pub fn get(&self, ix: usize) -> Option<Row> {
        let ix = match ix.checked_sub(self.lead.len()) {
            None => return self.lead.get(ix).copied(),
            Some(ix) => ix,
        };
        match ix.checked_sub(self.items.len()) {
            None => self.items.get(ix).copied().map(Row::Item),
            Some(ix) => self.trail.get(ix).copied(),
        }
    }

    /// Recomputes every row (another view was opened); returns the row count.
    pub fn rebuild(
        &mut self,
        lead: Vec<Row>,
        len: usize,
        visible: impl Fn(usize) -> bool,
        trail: Vec<Row>,
    ) -> usize {
        self.lead = lead;
        self.items = (0..len).filter(|&i| visible(i)).collect();
        self.trail = trail;
        self.synced = len;
        self.len()
    }

    /// Brings the rows up to `len` items. `dirty`: items changed in place since the last call.
    /// `relink`: the visibility of unchanged items may have changed too (a subagent launch
    /// arrived), so every seen item is checked again. Items are only ever appended, never
    /// removed. Without `relink`, `visible` runs only for dirty and new items.
    pub fn update(
        &mut self,
        lead: Vec<Row>,
        len: usize,
        dirty: &[usize],
        relink: bool,
        visible: impl Fn(usize) -> bool,
        trail: Vec<Row>,
    ) -> Vec<ListOp> {
        let mut ops = Vec::new();
        if lead != self.lead {
            ops.push(ListOp::Splice(0..self.lead.len(), lead.len()));
            self.lead = lead;
        }
        let base = self.lead.len();
        let seen = self.synced.min(len);

        let mut dirty: Vec<usize> = dirty.iter().copied().filter(|&i| i < seen).collect();
        dirty.sort_unstable();
        dirty.dedup();
        if relink {
            let now: Vec<usize> = (0..seen).filter(|&i| visible(i)).collect();
            ops.extend(
                diff_sorted(&self.items, &now)
                    .into_iter()
                    .map(|op| shift(op, base)),
            );
            self.items = now;
            for i in dirty {
                if let Ok(pos) = self.items.binary_search(&i) {
                    ops.push(ListOp::Remeasure(base + pos..base + pos + 1));
                }
            }
        } else {
            for i in dirty {
                let at = self.items.binary_search(&i);
                match (at, visible(i)) {
                    (Ok(pos), true) => ops.push(ListOp::Remeasure(base + pos..base + pos + 1)),
                    (Ok(pos), false) => {
                        self.items.remove(pos);
                        ops.push(ListOp::Splice(base + pos..base + pos + 1, 0));
                    }
                    (Err(pos), true) => {
                        self.items.insert(pos, i);
                        ops.push(ListOp::Splice(base + pos..base + pos, 1));
                    }
                    (Err(_), false) => {}
                }
            }
        }

        let at = base + self.items.len();
        let before = self.items.len();
        self.items.extend((seen..len).filter(|&i| visible(i)));
        if self.items.len() > before {
            ops.push(ListOp::Splice(at..at, self.items.len() - before));
        }
        self.synced = len;

        let at = base + self.items.len();
        if trail != self.trail {
            ops.push(ListOp::Splice(at..at + self.trail.len(), trail.len()));
            self.trail = trail;
        } else if !trail.is_empty() {
            // Their content follows state outside the items (diff summary, subagent result).
            ops.push(ListOp::Remeasure(at..at + trail.len()));
        }
        ops
    }
}

/// Notes that item `i` changed in place. Bounded by the number of distinct items, even while
/// the view is not rendered and streaming goes on.
pub(super) fn touch(dirty: &mut Vec<usize>, i: usize) {
    if dirty.last() == Some(&i) {
        return;
    }
    dirty.push(i);
    if dirty.len() > DIRTY_COMPACT {
        dirty.sort_unstable();
        dirty.dedup();
    }
}

const DIRTY_COMPACT: usize = 1024;

fn shift(op: ListOp, by: usize) -> ListOp {
    match op {
        ListOp::Splice(r, n) => ListOp::Splice(r.start + by..r.end + by, n),
        ListOp::Remeasure(r) => ListOp::Remeasure(r.start + by..r.end + by),
    }
}

/// The splices turning ascending `old` into ascending `new`, applied in order.
fn diff_sorted(old: &[usize], new: &[usize]) -> Vec<ListOp> {
    let mut ops = Vec::new();
    let (mut a, mut b, mut pos) = (0, 0, 0);
    // The pending run: where it starts, how many old rows it drops, how many it adds.
    let mut run: Option<(usize, usize, usize)> = None;
    while a < old.len() || b < new.len() {
        let (removed, added) = match (old.get(a), new.get(b)) {
            (Some(x), Some(y)) if x == y => {
                if let Some((start, r, n)) = run.take() {
                    ops.push(ListOp::Splice(start..start + r, n));
                }
                a += 1;
                b += 1;
                pos += 1;
                continue;
            }
            (Some(x), Some(y)) if x < y => (1, 0),
            (Some(_), None) => (1, 0),
            _ => (0, 1),
        };
        let run = run.get_or_insert((pos, 0, 0));
        run.1 += removed;
        run.2 += added;
        a += removed;
        b += added;
        pos += added;
    }
    if let Some((start, r, n)) = run {
        ops.push(ListOp::Splice(start..start + r, n));
    }
    ops
}

#[cfg(test)]
mod tests;
