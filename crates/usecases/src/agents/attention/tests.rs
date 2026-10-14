use super::{Attention, Observed};
use hangar_core::AgentState::*;

/// The badge to set and how many workspaces started waiting.
fn seen(badge: Option<usize>, newly_waiting: usize) -> (Option<usize>, usize) {
    (badge, newly_waiting)
}

fn summary(o: Observed<i32>) -> (Option<usize>, usize) {
    (o.badge, o.waiting.len())
}

#[test]
fn counts_approval_input_and_failed() {
    let mut a = Attention::new(true);
    let all = [
        (1, Approval),
        (2, Input),
        (3, Failed),
        (4, Review),
        (5, Working),
        (6, Idle),
    ];
    assert_eq!(summary(a.observe(all)), seen(Some(3), 3));
    // Same states: nothing to set, nothing new.
    assert_eq!(summary(a.observe(all)), seen(None, 0));
}

#[test]
fn only_new_waits_count() {
    let mut a = Attention::new(true);
    assert_eq!(
        summary(a.observe([(1, Working), (2, Approval)])),
        seen(Some(1), 1)
    );
    // 2 moves between waiting states: still the same wait.
    assert_eq!(
        summary(a.observe([(1, Working), (2, Failed)])),
        seen(None, 0)
    );
    assert_eq!(
        summary(a.observe([(1, Input), (2, Failed)])),
        seen(Some(2), 1)
    );
    // Back to work and waiting again is a new wait.
    assert_eq!(
        summary(a.observe([(1, Working), (2, Failed)])),
        seen(Some(1), 0)
    );
    assert_eq!(
        summary(a.observe([(1, Approval), (2, Failed)])),
        seen(Some(2), 1)
    );
    // A removed workspace drops out of the count.
    assert_eq!(summary(a.observe([(1, Approval)])), seen(Some(1), 0));
    assert_eq!(summary(a.observe([])), seen(Some(0), 0));
}

#[test]
fn badge_only_while_enabled() {
    let mut a = Attention::new(false);
    assert_eq!(summary(a.observe([(1, Approval)])), seen(None, 1));
    a.enabled = true;
    assert_eq!(summary(a.observe([(1, Approval)])), seen(Some(1), 0));
    a.enabled = false;
    assert_eq!(summary(a.observe([(1, Approval)])), seen(Some(0), 0));
    assert_eq!(
        summary(a.observe([(1, Approval), (2, Failed)])),
        seen(None, 1)
    );
}

#[test]
fn waits_carry_their_state() {
    let mut a = Attention::new(true);
    let o = a.observe([(1, Working), (2, Input), (3, Failed)]);
    assert_eq!(o.waiting, vec![(2, Input), (3, Failed)]);
    assert!(o.finished.is_empty());
}

#[test]
fn a_turn_end_is_finished_once() {
    let mut a = Attention::new(true);
    // First sight never counts as finished, even when idle or ready for review.
    assert!(
        a.observe([(1, Working), (2, Idle), (3, Review)])
            .finished
            .is_empty()
    );
    let o = a.observe([(1, Review), (2, Idle), (3, Review)]);
    assert_eq!(o.finished, vec![(1, Review)]);
    assert!(
        a.observe([(1, Review), (2, Idle), (3, Review)])
            .finished
            .is_empty()
    );
    assert!(a.observe([(1, Working), (2, Working)]).finished.is_empty());
    let o = a.observe([(1, Idle), (2, Approval)]);
    assert_eq!(o.finished, vec![(1, Idle)]);
    assert_eq!(o.waiting, vec![(2, Approval)]);
    // Leaving a wait is not finishing a turn.
    assert!(a.observe([(1, Idle), (2, Idle)]).finished.is_empty());
}

#[test]
fn finished_only_while_enabled() {
    let mut a = Attention::new(true);
    a.notify_finished = false;
    a.observe([(1, Working)]);
    assert!(a.observe([(1, Idle)]).finished.is_empty());
    a.notify_finished = true;
    a.observe([(1, Working)]);
    assert_eq!(a.observe([(1, Idle)]).finished, vec![(1, Idle)]);
}

#[test]
fn a_failure_is_a_wait_not_a_finish() {
    let mut a = Attention::new(true);
    a.observe([(1, Working)]);
    let o = a.observe([(1, Failed)]);
    assert_eq!(o.waiting, vec![(1, Failed)]);
    assert!(o.finished.is_empty());
}
