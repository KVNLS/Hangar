//! What agents finished: the sessions that ended on the local day, for the overview's
//! "Done today". Pure: the caller passes the clock and the local UTC offset.

use hangar_core::changes::{Cost, DiffStat};
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
    pub cost: Option<Cost>,
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
                cost: session.cost.clone(),
                ended_at,
            })
        })
        .collect();
    done.sort_by_key(|f| std::cmp::Reverse(f.ended_at));
    done
}

/// What today cost, per currency in first-seen order: the sessions that ended today as stored,
/// and the `live` ones with their latest cost (which wins when a session is in both).
pub fn cost_today(ended_today: &[Session], live: &[(SessionId, Cost)]) -> Vec<Cost> {
    let stored = ended_today
        .iter()
        .filter(|s| !live.iter().any(|(id, _)| *id == s.id))
        .filter_map(|s| s.cost.as_ref());
    sum_costs(stored.chain(live.iter().map(|(_, cost)| cost)))
}

/// Costs summed per currency, in first-seen order.
pub fn sum_costs<'a>(costs: impl IntoIterator<Item = &'a Cost>) -> Vec<Cost> {
    let mut totals: Vec<Cost> = Vec::new();
    for cost in costs {
        match totals.iter_mut().find(|t| t.currency == cost.currency) {
            Some(total) => total.amount += cost.amount,
            None => totals.push(cost.clone()),
        }
    }
    totals
}

#[cfg(test)]
mod tests;
