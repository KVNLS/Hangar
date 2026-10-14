//! Guard rails of the orchestration tools, through which a lead agent creates child workspaces
//! and drives them: an agent acts only on the workspaces its own workspace created, children
//! nest at most [`MAX_DEPTH`] levels, and a workspace has at most [`MAX_LIVE_CHILDREN`] active
//! children. Pure: the caller passes every workspace it knows.

use hangar_core::{Workspace, WorkspaceId, WorkspaceStatus};

use crate::batch::name_from_line;
use crate::workspaces::NewWorkspace;

/// Levels of children below a workspace nobody created: a child may create children, a
/// grandchild may not.
pub const MAX_DEPTH: usize = 2;
/// Active (not archived) children one workspace may have.
pub const MAX_LIVE_CHILDREN: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the task is empty")]
    EmptyTask,
    #[error(
        "this workspace was created {depth} levels below a lead workspace; workspaces that deep \
         cannot create workspaces (at most {MAX_DEPTH} levels)"
    )]
    TooDeep { depth: usize },
    #[error(
        "this workspace already has {MAX_LIVE_CHILDREN} active child workspaces; wait until the \
         user archives one"
    )]
    TooManyChildren,
    #[error("no workspace {0:?}; workspaces_list shows the ones this agent created")]
    UnknownChild(String),
    #[error("“{0}” was not created by this workspace's agent; it can only act on its own")]
    NotAChild(String),
    #[error("“{0}” is archived")]
    Archived(String),
    #[error("several child workspaces are named {0:?}; pass the id instead")]
    Ambiguous(String),
}

/// How many workspaces created `id`, walking up the parent links. A link to a deleted parent
/// still counts and ends the walk; a cycle cannot make it loop.
pub fn depth<'a>(all: impl IntoIterator<Item = &'a Workspace> + Clone, id: WorkspaceId) -> usize {
    let find = |id: WorkspaceId| all.clone().into_iter().find(|ws| ws.id == id);
    let limit = all.clone().into_iter().count();
    let mut depth = 0;
    let mut current = find(id).and_then(|ws| ws.parent);
    while let Some(parent) = current {
        if depth >= limit {
            break;
        }
        depth += 1;
        current = find(parent).and_then(|ws| ws.parent);
    }
    depth
}

/// The active workspaces `parent` created, oldest first.
pub fn children<'a>(
    all: impl IntoIterator<Item = &'a Workspace>,
    parent: WorkspaceId,
) -> Vec<&'a Workspace> {
    let mut list: Vec<&Workspace> = all
        .into_iter()
        .filter(|ws| ws.parent == Some(parent) && ws.status == WorkspaceStatus::Active)
        .collect();
    list.sort_by_key(|ws| ws.created_at);
    list
}

/// Whether the agent of `caller` may create one more child, with `in_flight` of its children
/// still being created.
pub fn check_create<'a>(
    all: impl IntoIterator<Item = &'a Workspace> + Clone,
    caller: WorkspaceId,
    in_flight: usize,
) -> Result<(), Refusal> {
    let depth = depth(all.clone(), caller);
    if depth >= MAX_DEPTH {
        return Err(Refusal::TooDeep { depth });
    }
    if children(all, caller).len() + in_flight >= MAX_LIVE_CHILDREN {
        return Err(Refusal::TooManyChildren);
    }
    Ok(())
}

/// The child of `caller` that `query` names: its id, else its exact name (ignoring case and
/// surrounding blanks). Archived children and other workspaces are refused.
pub fn find_child<'a>(
    all: impl IntoIterator<Item = &'a Workspace>,
    caller: WorkspaceId,
    query: &str,
) -> Result<&'a Workspace, Refusal> {
    let query = query.trim();
    let all: Vec<&Workspace> = all.into_iter().collect();
    let by_id = all.iter().find(|ws| ws.id.0.to_string() == query);
    let found = match by_id {
        Some(ws) => *ws,
        None => {
            let named: Vec<&Workspace> = all
                .iter()
                .copied()
                .filter(|ws| ws.name.trim().eq_ignore_ascii_case(query))
                .collect();
            // A name shared with other workspaces still finds the caller's own child.
            let own: Vec<&Workspace> = named
                .iter()
                .copied()
                .filter(|ws| ws.parent == Some(caller))
                .collect();
            let active: Vec<&Workspace> = own
                .iter()
                .copied()
                .filter(|ws| ws.status == WorkspaceStatus::Active)
                .collect();
            match (active.as_slice(), own.first(), named.first()) {
                ([one], _, _) => *one,
                ([_, _, ..], _, _) => return Err(Refusal::Ambiguous(query.to_owned())),
                ([], Some(archived), _) => *archived,
                ([], None, Some(other)) => *other,
                ([], None, None) => return Err(Refusal::UnknownChild(query.to_owned())),
            }
        }
    };
    if found.parent != Some(caller) {
        return Err(Refusal::NotAChild(found.name.clone()));
    }
    if found.status != WorkspaceStatus::Active {
        return Err(Refusal::Archived(found.name.clone()));
    }
    Ok(found)
}

/// The workspace to create for `task` under `parent`. It starts from the parent's own base
/// branch, so a child is reviewed and merged against the same branch as its lead. Without a
/// name, the task's first line names it.
pub fn child_workspace(
    parent: &Workspace,
    task: &str,
    name: Option<&str>,
) -> Result<NewWorkspace, Refusal> {
    let task = task.trim();
    let first_line = task.lines().map(str::trim).find(|line| !line.is_empty());
    let Some(first_line) = first_line else {
        return Err(Refusal::EmptyTask);
    };
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| name_from_line(first_line), str::to_owned);
    Ok(NewWorkspace {
        name,
        base: Some(parent.base.clone()),
        branch: None,
        task: Some(task.to_owned()),
        group: None,
        parent: Some(parent.id),
    })
}

#[cfg(test)]
mod tests;
