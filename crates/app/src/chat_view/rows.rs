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
mod tests {
    use std::cell::Cell;

    use super::{DIRTY_COMPACT, ListOp, Row, Rows, diff_sorted, touch};

    /// Applies `ops` to a model of the list's rows (`None`: a row the list must measure).
    fn apply(rows: &mut Vec<Option<usize>>, ops: &[ListOp]) {
        for op in ops {
            match op {
                ListOp::Splice(r, n) => {
                    rows.splice(r.clone(), std::iter::repeat_n(None, *n));
                }
                ListOp::Remeasure(r) => {
                    for row in &mut rows[r.clone()] {
                        *row = None;
                    }
                }
            }
        }
    }

    fn items(rows: &Rows) -> Vec<Row> {
        (0..rows.len()).filter_map(|i| rows.get(i)).collect()
    }

    #[test]
    fn touched_items_stay_bounded() {
        let mut dirty = Vec::new();
        for _ in 0..10 {
            touch(&mut dirty, 7);
        }
        assert_eq!(dirty, vec![7]);
        for n in 0..100_000 {
            touch(&mut dirty, n % 3);
        }
        assert!(dirty.len() <= DIRTY_COMPACT + 1);
        let mut seen = dirty.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, vec![0, 1, 2, 7]);
    }

    #[test]
    fn appended_items_are_one_splice() {
        let mut rows = Rows::default();
        assert_eq!(rows.rebuild(vec![], 3, |_| true, vec![]), 3);
        let ops = rows.update(vec![], 5, &[], false, |_| true, vec![]);
        assert_eq!(ops, vec![ListOp::Splice(3..3, 2)]);
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn streaming_text_remeasures_only_its_row() {
        let mut rows = Rows::default();
        rows.rebuild(vec![], 4, |i| i != 1, vec![Row::Working]);
        let ops = rows.update(vec![], 4, &[3, 3], false, |i| i != 1, vec![Row::Working]);
        assert_eq!(ops, vec![ListOp::Remeasure(2..3), ListOp::Remeasure(3..4)]);
        // A hidden item changing touches nothing but the trail.
        let ops = rows.update(vec![], 4, &[1], false, |i| i != 1, vec![]);
        assert_eq!(ops, vec![ListOp::Splice(3..4, 0)]);
    }

    #[test]
    fn dirty_items_can_appear_and_disappear() {
        let mut rows = Rows::default();
        rows.rebuild(vec![Row::Task], 5, |i: usize| i.is_multiple_of(2), vec![]);
        assert_eq!(
            items(&rows),
            vec![Row::Task, Row::Item(0), Row::Item(2), Row::Item(4)]
        );
        let ops = rows.update(vec![Row::Task], 5, &[1, 2], false, |i| i != 2, vec![]);
        assert_eq!(ops, vec![ListOp::Splice(2..2, 1), ListOp::Splice(3..4, 0)]);
        assert_eq!(
            items(&rows),
            vec![Row::Task, Row::Item(0), Row::Item(1), Row::Item(4)]
        );
    }

    #[test]
    fn lead_and_trail_changes_splice_their_rows() {
        let mut rows = Rows::default();
        rows.rebuild(vec![], 2, |_| true, vec![Row::Working]);
        let ops = rows.update(vec![Row::Task], 3, &[], false, |_| true, vec![Row::Done]);
        assert_eq!(
            ops,
            vec![
                ListOp::Splice(0..0, 1),
                ListOp::Splice(3..3, 1),
                ListOp::Splice(4..5, 1),
            ]
        );
        assert_eq!(
            items(&rows),
            vec![
                Row::Task,
                Row::Item(0),
                Row::Item(1),
                Row::Item(2),
                Row::Done
            ]
        );
        assert_eq!(rows.get(5), None);
    }

    #[test]
    fn relink_diffs_against_the_new_visibility() {
        let mut rows = Rows::default();
        rows.rebuild(vec![Row::Task], 6, |i| i < 3, vec![Row::Result]);
        let mut model: Vec<Option<usize>> = (0..rows.len()).map(Some).collect();
        let visible = |i: usize| i == 0 || i >= 4;
        let ops = rows.update(vec![Row::Task], 7, &[0], true, visible, vec![Row::Result]);
        apply(&mut model, &ops);
        assert_eq!(model.len(), rows.len());
        assert_eq!(
            items(&rows),
            vec![
                Row::Task,
                Row::Item(0),
                Row::Item(4),
                Row::Item(5),
                Row::Item(6),
                Row::Result
            ]
        );
        // The lead row was kept; the changed item 0 is measured again.
        assert_eq!(model[0], Some(0));
        assert_eq!(model[1], None);
    }

    #[test]
    fn diff_sorted_turns_old_into_new() {
        let cases: [(&[usize], &[usize]); 6] = [
            (&[], &[1, 2]),
            (&[1, 2], &[]),
            (&[1, 3, 5], &[1, 2, 3, 4, 5]),
            (&[1, 2, 3, 4, 5], &[2, 4]),
            (&[0, 2, 4, 6], &[1, 2, 5, 6, 7]),
            (&[3, 4], &[3, 4]),
        ];
        for (old, new) in cases {
            let mut model: Vec<Option<usize>> = old.iter().copied().map(Some).collect();
            apply(&mut model, &diff_sorted(old, new));
            let expect: Vec<Option<usize>> =
                new.iter().map(|i| old.contains(i).then_some(*i)).collect();
            assert_eq!(model, expect, "{old:?} -> {new:?}");
        }
        assert!(diff_sorted(&[3, 4], &[3, 4]).is_empty());
        assert_eq!(
            diff_sorted(&[1, 2, 3, 4], &[1, 4]),
            vec![ListOp::Splice(1..3, 0)]
        );
    }

    /// A long session: a chunk and a new item only look at those two items.
    #[test]
    fn sync_cost_follows_the_change_not_the_transcript() {
        const LEN: usize = 5_000;
        let calls = Cell::new(0usize);
        let visible = |i: usize| {
            calls.set(calls.get() + 1);
            !i.is_multiple_of(3)
        };
        let mut rows = Rows::default();
        rows.rebuild(vec![], LEN, visible, vec![Row::Working]);
        assert_eq!(calls.get(), LEN);

        calls.set(0);
        let last = LEN - 1;
        let ops = rows.update(vec![], LEN + 1, &[last], false, visible, vec![Row::Working]);
        assert_eq!(calls.get(), 2);
        let row = rows.len() - 3;
        assert_eq!(
            ops,
            vec![
                ListOp::Remeasure(row..row + 1),
                ListOp::Splice(row + 1..row + 1, 1),
                ListOp::Remeasure(row + 2..row + 3),
            ]
        );

        calls.set(0);
        let ops = rows.update(vec![], LEN + 1, &[], false, visible, vec![Row::Working]);
        assert_eq!(calls.get(), 0);
        assert_eq!(ops.len(), 1);
    }
}
