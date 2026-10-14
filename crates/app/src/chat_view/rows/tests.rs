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
