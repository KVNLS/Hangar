//! Several workspaces from one New workspace request: one per task line, one per agent (best
//! of N, linked as a group to compare), or both. Pure: the caller creates what is planned.

use hangar_core::{Workspace, WorkspaceGroupId, WorkspaceId};

use crate::workspaces::NewWorkspace;

/// Most workspaces one request may create, so a pasted list cannot start dozens of agents.
pub const MAX_BATCH: usize = 12;

/// Longest name taken from a task line, in characters.
const LINE_NAME_CHARS: usize = 40;

/// A New workspace request.
#[derive(Clone, Debug, Default)]
pub struct BatchRequest {
    /// Required unless `per_line`, where each line names its workspace.
    pub name: String,
    pub task: String,
    /// The agents to run, as (preset id, short label for names). Several make a group per task.
    pub agents: Vec<(String, String)>,
    /// Each non-empty line of `task` is its own workspace.
    pub per_line: bool,
    pub base: Option<String>,
    /// Only used for a single workspace: one branch name cannot be given twice.
    pub branch: Option<String>,
}

/// One workspace to create and the agent to start in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub workspace: NewWorkspace,
    pub agent: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    #[error("pick an agent")]
    NoAgent,
    #[error("workspace name is empty")]
    EmptyName,
    #[error("write one task per line")]
    NoTasks,
    #[error("{count} workspaces is more than {MAX_BATCH} at once")]
    TooMany { count: usize },
}

/// The workspaces `request` asks for, tasks first then agents, in the order they are listed.
pub fn plan(request: &BatchRequest) -> Result<Vec<Planned>, BatchError> {
    if request.agents.is_empty() {
        return Err(BatchError::NoAgent);
    }
    let tasks: Vec<(String, Option<String>)> = if request.per_line {
        let lines: Vec<(String, Option<String>)> = request
            .task
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| (name_from_line(line), Some(line.to_owned())))
            .collect();
        if lines.is_empty() {
            return Err(BatchError::NoTasks);
        }
        lines
    } else {
        let name = request.name.trim();
        if name.is_empty() {
            return Err(BatchError::EmptyName);
        }
        let task = request.task.trim();
        vec![(name.to_owned(), (!task.is_empty()).then(|| task.to_owned()))]
    };
    let count = tasks.len() * request.agents.len();
    if count > MAX_BATCH {
        return Err(BatchError::TooMany { count });
    }
    let several_agents = request.agents.len() > 1;
    let branch = if count == 1 {
        request.branch.clone()
    } else {
        None
    };
    let mut planned = Vec::with_capacity(count);
    for (name, task) in tasks {
        let group = several_agents.then(WorkspaceGroupId::new);
        for (agent, label) in &request.agents {
            let name = if several_agents {
                format!("{name} · {label}")
            } else {
                name.clone()
            };
            planned.push(Planned {
                workspace: NewWorkspace {
                    name,
                    base: request.base.clone(),
                    branch: branch.clone(),
                    task: task.clone(),
                    group,
                    parent: None,
                },
                agent: agent.clone(),
            });
        }
    }
    Ok(planned)
}

/// A workspace name from a task line: cut at a word boundary near [`LINE_NAME_CHARS`].
pub fn name_from_line(line: &str) -> String {
    if line.chars().count() <= LINE_NAME_CHARS {
        return line.to_owned();
    }
    // One character more, so a word ending right at the limit is kept whole.
    let cut: String = line.chars().take(LINE_NAME_CHARS + 1).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(space) if space > LINE_NAME_CHARS / 2 => &cut[..space],
        _ => {
            let end = cut
                .char_indices()
                .nth(LINE_NAME_CHARS)
                .map_or(cut.len(), |(i, _)| i);
            &cut[..end]
        }
    };
    format!("{}…", cut.trim_end())
}

/// The active attempts of `group`, oldest first.
pub fn attempts<'a>(
    workspaces: impl IntoIterator<Item = &'a Workspace>,
    group: WorkspaceGroupId,
) -> Vec<&'a Workspace> {
    let mut list: Vec<&Workspace> = workspaces
        .into_iter()
        .filter(|ws| ws.group == Some(group) && ws.status == hangar_core::WorkspaceStatus::Active)
        .collect();
    list.sort_by_key(|ws| ws.created_at);
    list
}

/// "Keep this one": the other active attempts of `keep`'s group, to archive.
pub fn others_to_archive<'a>(
    workspaces: impl IntoIterator<Item = &'a Workspace>,
    keep: &Workspace,
) -> Vec<WorkspaceId> {
    let Some(group) = keep.group else {
        return Vec::new();
    };
    attempts(workspaces, group)
        .into_iter()
        .filter(|ws| ws.id != keep.id)
        .map(|ws| ws.id)
        .collect()
}

#[cfg(test)]
mod tests;
