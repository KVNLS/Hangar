use super::*;

fn entry(level: LogLevel, msg: &str) -> LogEntry {
    LogEntry::new(level, msg)
}

fn texts(m: &LogModel) -> Vec<String> {
    (0..m.len())
        .filter_map(|i| m.get(i).map(|l| l.text.clone()))
        .collect()
}

#[test]
fn ring_buffer_discards_oldest() {
    let mut m = LogModel::new(3);
    m.push((0..5).map(|i| entry(LogLevel::Info, &format!("m{i}"))));
    assert_eq!(m.total(), 3);
    assert_eq!(m.len(), 3);
    assert!(m.get(0).is_some_and(|l| l.text.ends_with("m2")));
    assert!(m.get(2).is_some_and(|l| l.text.ends_with("m4")));
    assert!(m.get(3).is_none());
}

#[test]
fn widest_line_follows_pushes_eviction_and_filters() {
    let mut m = LogModel::new(3);
    assert_eq!(m.widest_index(), None);
    m.push([
        entry(LogLevel::Info, "a very long line indeed"),
        entry(LogLevel::Error, "short"),
        entry(LogLevel::Info, "medium line"),
    ]);
    assert_eq!(m.widest_index(), Some(0));
    // The widest line is evicted: the next widest takes over.
    m.push([entry(LogLevel::Info, "x")]);
    let widest = m
        .widest_index()
        .and_then(|i| m.get(i))
        .map(|l| l.text.clone());
    assert!(widest.is_some_and(|t| t.ends_with("medium line")));
    // Only visible lines count.
    m.set_min_level(LogLevel::Error);
    let widest = m
        .widest_index()
        .and_then(|i| m.get(i))
        .map(|l| l.text.clone());
    assert!(widest.is_some_and(|t| t.ends_with("short")));
    m.clear();
    assert_eq!(m.widest_index(), None);
}

#[test]
fn level_and_query_filter_with_eviction() {
    let mut m = LogModel::new(4);
    m.push([
        entry(LogLevel::Debug, "alpha"),
        entry(LogLevel::Error, "Beta"),
        entry(LogLevel::Info, "gamma"),
        entry(LogLevel::Error, "delta"),
    ]);
    m.set_min_level(LogLevel::Warn);
    assert_eq!(m.len(), 2);
    m.set_query(" BETA ");
    assert_eq!(m.len(), 1);
    m.set_query("");
    // Evicts "alpha" (not visible), then "Beta" (visible).
    m.push([entry(LogLevel::Fault, "eps")]);
    m.push([entry(LogLevel::Warn, "zeta")]);
    assert_eq!(m.total(), 4);
    let t = texts(&m);
    assert_eq!(t.len(), 3);
    assert!(t[0].ends_with("delta") && t[2].ends_with("zeta"));
    m.set_min_level(LogLevel::Verbose);
    assert_eq!(m.len(), 4);
}

#[test]
fn push_reports_visible_count_and_clear_empties() {
    let mut m = LogModel::new(10);
    m.set_min_level(LogLevel::Error);
    assert_eq!(
        m.push([entry(LogLevel::Info, "a"), entry(LogLevel::Error, "b")]),
        1
    );
    m.clear();
    assert_eq!((m.len(), m.total()), (0, 0));
}

#[test]
fn warning_and_error_counts_follow_the_ring_buffer() {
    let mut m = LogModel::new(3);
    assert_eq!(m.counts(), LevelCounts::default());
    m.push([
        entry(LogLevel::Warn, "w1"),
        entry(LogLevel::Error, "e1"),
        entry(LogLevel::Info, "i1"),
    ]);
    assert_eq!(
        m.counts(),
        LevelCounts {
            warnings: 1,
            errors: 1
        }
    );
    // Filters do not change the counts.
    m.set_min_level(LogLevel::Fault);
    m.set_query("nothing matches");
    // Evicts the warning, then the error; a fault counts as an error.
    m.push([entry(LogLevel::Fault, "f1"), entry(LogLevel::Debug, "d1")]);
    assert_eq!(
        m.counts(),
        LevelCounts {
            warnings: 0,
            errors: 1
        }
    );
    assert_eq!(m.counts().summary(), "0 warnings · 1 error");
    m.push([entry(LogLevel::Warn, "w2"), entry(LogLevel::Warn, "w3")]);
    assert_eq!(m.counts().summary(), "2 warnings · 0 errors");
    m.clear();
    assert_eq!(m.counts(), LevelCounts::default());
}

#[test]
fn follow_tracks_where_the_user_scrolled() {
    let mut f = Follow::default();
    assert!(f.is_on());
    f.user_scrolled(Some(false));
    assert!(!f.is_on());
    f.user_scrolled(Some(true));
    assert!(f.is_on());
    f.user_scrolled(Some(false));
    f.user_scrolled(None);
    assert!(f.is_on());
}

#[test]
fn selection_and_copy() {
    assert_eq!(select(None, 5, false), (5, 5..=5));
    assert_eq!(select(Some(2), 5, true), (2, 2..=5));
    assert_eq!(select(Some(7), 5, true), (7, 5..=7));
    assert_eq!(select(Some(2), 5, false), (5, 5..=5));
    let mut m = LogModel::new(10);
    m.push((0..4).map(|i| entry(LogLevel::Info, &format!("m{i}"))));
    assert_eq!(m.text(None).lines().count(), 4);
    let part = m.text(Some(&(1..=2)));
    assert_eq!(part.lines().count(), 2);
    assert!(part.lines().next().is_some_and(|l| l.ends_with("m1")));
}

#[test]
fn formats_android_and_ios_entries() {
    let mut e = entry(LogLevel::Warn, "boom");
    e.timestamp = "10-04 12:34:56.789".into();
    e.tag = "ActivityManager".into();
    e.pid = Some(42);
    assert_eq!(format_entry(&e), "12:34:56.789 W ActivityManager[42] boom");
    e.timestamp = "2026-10-04 12:34:56.123456+0200".into();
    e.process = "SpringBoard".into();
    assert!(format_entry(&e).starts_with("12:34:56.123 W SpringBoard[42] "));
    e.message = "a\tb".into();
    assert!(format_entry(&e).ends_with("a b"));
}
