use std::path::PathBuf;

use hangar_core::ProjectId;

use super::*;
use crate::sessions::stopped;

const HOUR: i64 = 3_600_000;
/// 2026-10-08T00:00:00Z.
const MIDNIGHT_UTC: i64 = 1_791_417_600_000;

fn workspace(name: &str, task: Option<&str>) -> Workspace {
    Workspace {
        id: WorkspaceId::new(),
        project_id: ProjectId::new(),
        name: name.to_owned(),
        slug: name.to_owned(),
        branch: format!("hangar/{name}"),
        base: "main".to_owned(),
        worktree_path: PathBuf::from(format!("/wt/{name}")),
        status: WorkspaceStatus::Active,
        created_at: Timestamp(0),
        task: task.map(str::to_owned),
        group: None,
        parent: None,
    }
}

fn ended(ws: &Workspace, status: SessionStatus, at: i64) -> Session {
    let mut s = Session::new(ws.id, AgentKind::Claude);
    s.status = status;
    s.ended_at = Some(Timestamp(at));
    s
}

#[test]
fn the_local_day_follows_the_offset() {
    let now = Timestamp(MIDNIGHT_UTC + 10 * HOUR);
    assert_eq!(local_day_start(now, 0), Timestamp(MIDNIGHT_UTC));
    // UTC+2: local midnight was 22:00 UTC the day before.
    assert_eq!(
        local_day_start(now, 2 * 3600),
        Timestamp(MIDNIGHT_UTC - 2 * HOUR)
    );
    // UTC-7 at 03:00 UTC is still the previous local day, which started at 07:00 UTC.
    let early = Timestamp(MIDNIGHT_UTC + 3 * HOUR);
    assert_eq!(
        local_day_start(early, -7 * 3600),
        Timestamp(MIDNIGHT_UTC - 17 * HOUR)
    );
    assert_eq!(
        clock_label(Timestamp(MIDNIGHT_UTC + 9 * HOUR + 5 * 60_000), 3600),
        "10:05"
    );
    assert_eq!(clock_label(Timestamp(MIDNIGHT_UTC), -3600), "23:00");
}

#[test]
fn outcomes_tell_exits_failures_and_stops_apart() {
    assert_eq!(Outcome::of(&SessionStatus::Running), None);
    assert_eq!(
        Outcome::of(&SessionStatus::Exited { code: 0 }),
        Some(Outcome::Done)
    );
    assert_eq!(
        Outcome::of(&SessionStatus::Exited { code: 1 }),
        Some(Outcome::Failed)
    );
    assert_eq!(
        Outcome::of(&SessionStatus::Failed {
            message: "spawn failed".into()
        }),
        Some(Outcome::Failed)
    );
    assert_eq!(
        Outcome::of(&stopped("workspace archived")),
        Some(Outcome::Stopped)
    );
}

#[test]
fn done_today_keeps_todays_ends_with_their_workspace_latest_first() {
    let login = workspace("login", Some("Fix the login crash\nwith a test"));
    let mut docs = workspace("docs", None);
    docs.status = WorkspaceStatus::Archived;
    let gone = workspace("gone", None);
    let offset = 2 * 3600;
    // Local midnight is 22:00 UTC the day before.
    let today = MIDNIGHT_UTC - 2 * HOUR;
    let mut with_diff = ended(&login, SessionStatus::Exited { code: 0 }, today + HOUR);
    with_diff.changes = Some(DiffStat {
        files: 2,
        insertions: 10,
        deletions: 1,
    });
    let sessions = [
        with_diff.clone(),
        ended(&docs, stopped("workspace archived"), today + 3 * HOUR),
        ended(&login, SessionStatus::Exited { code: 0 }, today - 1),
        ended(&gone, SessionStatus::Exited { code: 0 }, today + 2 * HOUR),
        Session::new(login.id, AgentKind::Codex),
    ];
    let known = [&login, &docs];
    let lookup = |id| known.iter().copied().find(|w| w.id == id);
    let done = done_today(&sessions, lookup, Timestamp(today + 5 * HOUR), offset);

    assert_eq!(done.len(), 2);
    assert_eq!(done[0].workspace, docs.id);
    assert_eq!(done[0].title, "docs");
    assert!(done[0].archived);
    assert_eq!(done[0].outcome, Outcome::Stopped);
    assert_eq!(
        done[1],
        Finished {
            session: with_diff.id,
            workspace: login.id,
            agent: AgentKind::Claude,
            title: "Fix the login crash".to_owned(),
            archived: false,
            outcome: Outcome::Done,
            changes: with_diff.changes,
            cost: None,
            ended_at: Timestamp(today + HOUR),
        }
    );
}

#[test]
fn cost_today_counts_each_session_once() {
    let ws = workspace("w", None);
    let usd = |amount| Cost {
        amount,
        currency: "USD".into(),
    };
    let mut first = ended(&ws, SessionStatus::Exited { code: 0 }, MIDNIGHT_UTC + 1);
    first.cost = Some(usd(1.0));
    let mut still_live = ended(&ws, SessionStatus::Exited { code: 0 }, MIDNIGHT_UTC + 2);
    still_live.cost = Some(usd(0.5));
    let no_cost = ended(&ws, SessionStatus::Exited { code: 0 }, MIDNIGHT_UTC + 3);
    let eur = Cost {
        amount: 2.0,
        currency: "EUR".into(),
    };
    let live = [(still_live.id, usd(0.75)), (SessionId::new(), eur.clone())];
    let total = cost_today(&[first, still_live, no_cost], &live);
    assert_eq!(total, [usd(1.75), eur]);
    assert!(cost_today(&[], &[]).is_empty());
}
