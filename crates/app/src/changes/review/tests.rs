use super::*;
use crate::changes::model::short_sha;
use hangar_core::changes::ChangeKind;

fn change(staged: bool, unstaged: bool) -> ChangedPath {
    ChangedPath {
        path: "new.rs".into(),
        orig_path: Some("old.rs".into()),
        kind: ChangeKind::Renamed,
        staged,
        unstaged,
    }
}

#[test]
fn stage_states() {
    assert_eq!(StageState::of(None), StageState::Committed);
    assert_eq!(
        StageState::of(Some(&change(true, false))),
        StageState::Staged
    );
    assert_eq!(
        StageState::of(Some(&change(true, true))),
        StageState::Partial
    );
    assert_eq!(
        StageState::of(Some(&change(false, true))),
        StageState::Unstaged
    );
    assert!(StageState::Staged.checked() && !StageState::Partial.checked());
    assert_eq!(
        stage_paths(&change(true, false)),
        [PathBuf::from("new.rs"), PathBuf::from("old.rs")]
    );
}

#[test]
fn base_labels() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(base_label(Some("main"), sha), "main");
    assert_eq!(base_label(Some(" "), sha), short_sha(sha));
    assert_eq!(base_label(None, sha), short_sha(sha));
}

#[test]
fn commit_and_push_text() {
    assert_eq!(commit_blocker("msg", 0), Some("Stage files to commit"));
    assert_eq!(commit_blocker(" \n", 2), Some("Write a commit subject"));
    assert_eq!(commit_blocker("msg", 1), None);

    let claude = coauthor_trailer(&AgentKind::Claude);
    assert_eq!(
        claude.as_deref(),
        Some("Co-authored-by: Claude <noreply@anthropic.com>")
    );
    assert!(
        coauthor_trailer(&AgentKind::Codex)
            .unwrap()
            .contains("Codex <")
    );
    assert_eq!(coauthor_trailer(&AgentKind::Shell), None);
    assert_eq!(coauthor_trailer(&AgentKind::Custom("aider".into())), None);
    let trailer = claude.as_deref();
    assert_eq!(commit_message(" Fix it ", "", None), "Fix it");
    assert_eq!(
        commit_message("Fix it", " Why.\n", trailer),
        "Fix it\n\nWhy.\n\nCo-authored-by: Claude <noreply@anthropic.com>"
    );
    assert_eq!(
        commit_message("Fix it", "", trailer),
        "Fix it\n\nCo-authored-by: Claude <noreply@anthropic.com>"
    );
    let signed = "Why.\n\nco-authored-by: Claude <noreply@anthropic.com>";
    assert_eq!(
        commit_message("Fix it", signed, trailer),
        format!("Fix it\n\n{signed}")
    );

    let mut status = Status::default();
    assert_eq!(push_label(&status), "Publish");
    status.branch = Some("hangar/x".into());
    status.upstream = Some("origin/main".into());
    status.ahead = 3;
    assert_eq!(push_label(&status), "Publish");
    status.ahead = 0;
    status.upstream = Some("origin/hangar/x".into());
    assert_eq!(push_label(&status), "Push");
    assert!(push_tooltip(&status).contains("Up to date"));
    status.ahead = 2;
    status.behind = 1;
    assert_eq!(push_label(&status), "Push ↑2 ↓1");
    assert!(push_tooltip(&status).starts_with("2 commit(s) to push, 1"));
}

#[test]
fn pr_and_merge_text() {
    assert_eq!(local_base("origin/main"), "main");
    assert_eq!(local_base("main"), "main");
    assert_eq!(local_base("feature/origin/x"), "feature/origin/x");
    assert_eq!(default_pr_title(Some(" Fix login "), "ws"), "Fix login");
    assert_eq!(default_pr_title(Some(""), "ws"), "ws");
    assert_eq!(default_pr_title(None, "ws"), "ws");
    assert_eq!(
        default_merge_message(MergeStrategy::NoFf, "hangar/x", "main", "T"),
        "Merge branch 'hangar/x' into main"
    );
    assert_eq!(
        default_merge_message(MergeStrategy::Squash, "hangar/x", "main", "T"),
        "T"
    );
    assert!(gh_blocker(&GhStatus::Missing).is_some());
    assert!(
        gh_blocker(&GhStatus::Installed {
            authenticated: false,
            account: None
        })
        .unwrap()
        .contains("gh auth login")
    );
    assert!(
        gh_blocker(&GhStatus::Installed {
            authenticated: true,
            account: None
        })
        .is_none()
    );
    let pr = PrInfo {
        number: 3,
        url: "u".into(),
        state: "OPEN".into(),
        title: "t".into(),
    };
    assert_eq!(pr_label(&pr), "PR #3 opened");
    let merged = PrInfo {
        state: "MERGED".into(),
        ..pr
    };
    assert_eq!(pr_label(&merged), "PR #3 merged");
    assert_eq!(pr_number("https://github.com/o/r/pull/42"), Some(42));
    assert_eq!(pr_number("https://github.com/o/r/pull/42/"), Some(42));
    assert_eq!(pr_number("https://github.com/o/r/issues/42"), None);
    assert_eq!(
        pr_opened_text("https://github.com/o/r/pull/7", "hangar/x", "main"),
        "Opened PR #7 · hangar/x → main"
    );
    assert_eq!(
        pr_opened_text("https://example.com/x", "b", "main"),
        "Opened https://example.com/x · b → main"
    );
}

#[test]
fn error_wording() {
    let conflicts = anyhow::Error::new(ReviewError::MergeConflicts {
        branch: "hangar/x".into(),
        base: "main".into(),
        paths: (0..10).map(|i| PathBuf::from(format!("f{i}"))).collect(),
    });
    let text = describe(&conflicts);
    assert!(
        text.contains("f0, f1") && text.contains("and 2 more"),
        "{text}"
    );
    assert!(is_merge_refusal(&conflicts));
    let ready = anyhow::Error::new(ReviewError::BaseNotReady {
        reason: "dirty".into(),
    });
    assert!(is_merge_refusal(&ready));
    assert!(describe(&ready).contains("dirty"));
    assert!(!is_merge_refusal(&anyhow::anyhow!("other")));
}
