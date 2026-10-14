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
fn name_from_line(line: &str) -> String {
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
mod tests {
    use super::*;

    fn agents(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter()
            .map(|id| ((*id).to_owned(), id.to_uppercase()))
            .collect()
    }

    fn request(name: &str, task: &str, ids: &[&str], per_line: bool) -> BatchRequest {
        BatchRequest {
            name: name.into(),
            task: task.into(),
            agents: agents(ids),
            per_line,
            ..BatchRequest::default()
        }
    }

    #[test]
    fn one_agent_one_task_is_a_plain_workspace() {
        let mut r = request(" Login ", "  Fix it ", &["claude"], false);
        r.branch = Some("fix/login".into());
        r.base = Some("develop".into());
        let planned = plan(&r).unwrap();
        assert_eq!(
            planned,
            [Planned {
                workspace: NewWorkspace {
                    name: "Login".into(),
                    base: Some("develop".into()),
                    branch: Some("fix/login".into()),
                    task: Some("Fix it".into()),
                    group: None,
                    parent: None,
                },
                agent: "claude".into(),
            }]
        );
        let blank = plan(&request("Login", "  ", &["claude"], false)).unwrap();
        assert_eq!(blank[0].workspace.task, None);
    }

    #[test]
    fn several_agents_share_one_group_and_get_named_after_the_agent() {
        let mut r = request("Login", "Fix it", &["claude", "codex"], false);
        r.branch = Some("fix/login".into());
        let planned = plan(&r).unwrap();
        let names: Vec<_> = planned.iter().map(|p| p.workspace.name.as_str()).collect();
        assert_eq!(names, ["Login · CLAUDE", "Login · CODEX"]);
        assert!(planned[0].workspace.group.is_some());
        assert_eq!(planned[0].workspace.group, planned[1].workspace.group);
        // One branch name cannot be used twice.
        assert!(planned.iter().all(|p| p.workspace.branch.is_none()));
        assert!(
            planned
                .iter()
                .all(|p| p.workspace.task.as_deref() == Some("Fix it"))
        );
    }

    #[test]
    fn one_workspace_per_line_ignores_the_name_and_blank_lines() {
        let planned = plan(&request(
            "ignored",
            "Fix login\n\n  Add tests  \n",
            &["claude"],
            true,
        ))
        .unwrap();
        let got: Vec<_> = planned
            .iter()
            .map(|p| (p.workspace.name.as_str(), p.workspace.task.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("Fix login", Some("Fix login")),
                ("Add tests", Some("Add tests"))
            ]
        );
        assert!(planned.iter().all(|p| p.workspace.group.is_none()));
    }

    #[test]
    fn lines_times_agents_make_one_group_per_line() {
        let planned = plan(&request("", "A\nB", &["claude", "codex"], true)).unwrap();
        assert_eq!(planned.len(), 4);
        assert_eq!(planned[0].workspace.group, planned[1].workspace.group);
        assert_eq!(planned[2].workspace.group, planned[3].workspace.group);
        assert_ne!(planned[0].workspace.group, planned[2].workspace.group);
    }

    #[test]
    fn refusals() {
        assert_eq!(
            plan(&request("x", "", &[], false)),
            Err(BatchError::NoAgent)
        );
        assert_eq!(
            plan(&request(" ", "task", &["a"], false)),
            Err(BatchError::EmptyName)
        );
        assert_eq!(
            plan(&request("x", " \n ", &["a"], true)),
            Err(BatchError::NoTasks)
        );
        let many = "t\n".repeat(5);
        assert_eq!(
            plan(&request("", &many, &["a", "b", "c"], true)),
            Err(BatchError::TooMany { count: 15 })
        );
    }

    #[test]
    fn long_lines_are_cut_at_a_word() {
        let line = "Rewrite the onboarding flow so new users see the permissions screen first";
        assert_eq!(
            name_from_line(line),
            "Rewrite the onboarding flow so new users…"
        );
        let word = "x".repeat(60);
        assert_eq!(name_from_line(&word), format!("{}…", "x".repeat(40)));
    }

    fn attempt(group: Option<WorkspaceGroupId>, at: i64, active: bool) -> Workspace {
        Workspace {
            id: WorkspaceId::new(),
            project_id: hangar_core::ProjectId::new(),
            name: format!("ws{at}"),
            slug: format!("ws{at}"),
            branch: format!("hangar/ws{at}"),
            base: "main".into(),
            worktree_path: format!("/wt/ws{at}").into(),
            status: if active {
                hangar_core::WorkspaceStatus::Active
            } else {
                hangar_core::WorkspaceStatus::Archived
            },
            created_at: hangar_core::Timestamp(at),
            task: None,
            group,
            parent: None,
        }
    }

    #[test]
    fn keeping_one_attempt_archives_the_other_active_ones() {
        let group = WorkspaceGroupId::new();
        let all = [
            attempt(Some(group), 3, true),
            attempt(Some(group), 1, true),
            attempt(Some(group), 2, false),
            attempt(Some(WorkspaceGroupId::new()), 4, true),
            attempt(None, 5, true),
        ];
        let listed: Vec<i64> = attempts(&all, group)
            .iter()
            .map(|ws| ws.created_at.0)
            .collect();
        assert_eq!(listed, [1, 3]);
        assert_eq!(others_to_archive(&all, &all[1]), [all[0].id]);
        assert!(others_to_archive(&all, &all[4]).is_empty());
    }
}
