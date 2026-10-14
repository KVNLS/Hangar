//! What agents finished: the sessions that ended on the local day, for the overview's
//! "Done today". Pure: the caller passes the clock and the local UTC offset.

use hangar_core::changes::DiffStat;
use hangar_core::{
    AgentKind, Session, SessionId, SessionStatus, Timestamp, Workspace, WorkspaceId,
    WorkspaceStatus,
};

use crate::sessions::is_stopped;

const DAY_MS: i64 = 86_400_000;

/// How a session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The agent exited with status 0.
    Done,
    /// It failed to start, crashed or exited with an error.
    Failed,
    /// Hangar stopped it (archive, replaced, closed while starting).
    Stopped,
}

impl Outcome {
    /// `None` while the session runs.
    pub fn of(status: &SessionStatus) -> Option<Self> {
        match status {
            SessionStatus::Starting | SessionStatus::Running => None,
            SessionStatus::Exited { code: 0 } => Some(Self::Done),
            status if is_stopped(status) => Some(Self::Stopped),
            SessionStatus::Exited { .. } | SessionStatus::Failed { .. } => Some(Self::Failed),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "Exited",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }
}

/// One session that ended, with what the overview shows of its workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct Finished {
    pub session: SessionId,
    pub workspace: WorkspaceId,
    pub agent: AgentKind,
    /// The workspace's task title, else its name.
    pub title: String,
    pub archived: bool,
    pub outcome: Outcome,
    pub changes: Option<DiffStat>,
    pub ended_at: Timestamp,
}

/// Start of the local day containing `now`, local time being `utc_offset_secs` ahead of UTC.
pub fn local_day_start(now: Timestamp, utc_offset_secs: i32) -> Timestamp {
    let offset = i64::from(utc_offset_secs) * 1000;
    let local = now.0 + offset;
    Timestamp(local - local.rem_euclid(DAY_MS) - offset)
}

/// Local `HH:MM` of `at`.
pub fn clock_label(at: Timestamp, utc_offset_secs: i32) -> String {
    let local = at.0 + i64::from(utc_offset_secs) * 1000;
    let minutes = local.rem_euclid(DAY_MS) / 60_000;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The sessions that ended on the local day of `now`, the latest first. Sessions whose
/// workspace `workspace` does not know (deleted) are left out.
pub fn done_today<'a>(
    sessions: &[Session],
    workspace: impl Fn(WorkspaceId) -> Option<&'a Workspace>,
    now: Timestamp,
    utc_offset_secs: i32,
) -> Vec<Finished> {
    let start = local_day_start(now, utc_offset_secs);
    let end = Timestamp(start.0 + DAY_MS);
    let mut done: Vec<Finished> = sessions
        .iter()
        .filter_map(|session| {
            let ended_at = session.ended_at.filter(|at| *at >= start && *at < end)?;
            let outcome = Outcome::of(&session.status)?;
            let ws = workspace(session.workspace_id)?;
            Some(Finished {
                session: session.id,
                workspace: ws.id,
                agent: session.agent.clone(),
                title: ws.title().to_owned(),
                archived: ws.status == WorkspaceStatus::Archived,
                outcome,
                changes: session.changes,
                ended_at,
            })
        })
        .collect();
    done.sort_by_key(|f| std::cmp::Reverse(f.ended_at));
    done
}

#[cfg(test)]
mod tests {
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
                ended_at: Timestamp(today + HOUR),
            }
        );
    }
}
