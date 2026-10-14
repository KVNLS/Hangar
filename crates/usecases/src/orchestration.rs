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
mod tests {
    use super::*;
    use hangar_core::{ProjectId, Timestamp};

    fn ws(name: &str, parent: Option<&Workspace>, at: i64) -> Workspace {
        Workspace {
            id: WorkspaceId::new(),
            project_id: ProjectId::new(),
            name: name.into(),
            slug: name.to_lowercase(),
            branch: format!("hangar/{}", name.to_lowercase()),
            base: "develop".into(),
            worktree_path: format!("/wt/{name}").into(),
            status: WorkspaceStatus::Active,
            created_at: Timestamp(at),
            task: None,
            group: None,
            parent: parent.map(|p| p.id),
        }
    }

    #[test]
    fn depth_counts_parents_and_survives_cycles_and_deleted_parents() {
        let lead = ws("Lead", None, 1);
        let child = ws("Child", Some(&lead), 2);
        let grandchild = ws("Grandchild", Some(&child), 3);
        let all = [lead.clone(), child.clone(), grandchild.clone()];
        assert_eq!(depth(&all, lead.id), 0);
        assert_eq!(depth(&all, child.id), 1);
        assert_eq!(depth(&all, grandchild.id), 2);
        // The lead was deleted: the child was still created by an agent.
        assert_eq!(depth(&all[1..], child.id), 1);
        assert_eq!(depth(&all[1..], grandchild.id), 2);

        let mut a = ws("A", None, 1);
        let b = ws("B", Some(&a), 2);
        a.parent = Some(b.id);
        let cycle = [a.clone(), b];
        assert!(depth(&cycle, a.id) <= cycle.len());
    }

    #[test]
    fn a_child_may_create_a_grandchild_may_not() {
        let lead = ws("Lead", None, 1);
        let child = ws("Child", Some(&lead), 2);
        let grandchild = ws("Grandchild", Some(&child), 3);
        let all = [lead.clone(), child.clone(), grandchild.clone()];
        assert_eq!(check_create(&all, lead.id, 0), Ok(()));
        assert_eq!(check_create(&all, child.id, 0), Ok(()));
        assert_eq!(
            check_create(&all, grandchild.id, 0),
            Err(Refusal::TooDeep { depth: 2 })
        );
    }

    #[test]
    fn at_most_eight_active_children() {
        let lead = ws("Lead", None, 0);
        let mut all = vec![lead.clone()];
        for i in 1..=MAX_LIVE_CHILDREN as i64 {
            all.push(ws(&format!("C{i}"), Some(&lead), i));
        }
        assert_eq!(
            check_create(&all, lead.id, 0),
            Err(Refusal::TooManyChildren)
        );
        all[1].status = WorkspaceStatus::Archived;
        assert_eq!(check_create(&all, lead.id, 0), Ok(()));
        // Creates still running count too.
        assert_eq!(
            check_create(&all, lead.id, 1),
            Err(Refusal::TooManyChildren)
        );
        assert_eq!(children(&all, lead.id).len(), MAX_LIVE_CHILDREN - 1);
    }

    #[test]
    fn children_are_the_active_ones_oldest_first() {
        let lead = ws("Lead", None, 0);
        let other = ws("Other", None, 0);
        let mut archived = ws("Old", Some(&lead), 1);
        archived.status = WorkspaceStatus::Archived;
        let all = [
            lead.clone(),
            ws("Second", Some(&lead), 3),
            ws("First", Some(&lead), 2),
            archived,
            ws("Theirs", Some(&other), 1),
        ];
        let names: Vec<&str> = children(&all, lead.id)
            .iter()
            .map(|w| w.name.as_str())
            .collect();
        assert_eq!(names, ["First", "Second"]);
    }

    #[test]
    fn an_agent_finds_only_its_own_active_children() {
        let lead = ws("Lead", None, 0);
        let rival = ws("Rival", None, 0);
        let mine = ws("Fix login", Some(&lead), 1);
        let theirs = ws("Add tests", Some(&rival), 2);
        let mut old = ws("Old", Some(&lead), 3);
        old.status = WorkspaceStatus::Archived;
        let all = [
            lead.clone(),
            rival.clone(),
            mine.clone(),
            theirs.clone(),
            old.clone(),
        ];
        assert_eq!(
            find_child(&all, lead.id, &mine.id.to_string()).map(|w| w.id),
            Ok(mine.id)
        );
        assert_eq!(
            find_child(&all, lead.id, " fix LOGIN ").map(|w| w.id),
            Ok(mine.id)
        );
        assert_eq!(
            find_child(&all, lead.id, &theirs.id.to_string()),
            Err(Refusal::NotAChild("Add tests".into()))
        );
        assert_eq!(
            find_child(&all, lead.id, "Add tests"),
            Err(Refusal::NotAChild("Add tests".into()))
        );
        // The caller itself is not its own child.
        assert_eq!(
            find_child(&all, lead.id, "Lead"),
            Err(Refusal::NotAChild("Lead".into()))
        );
        assert_eq!(
            find_child(&all, lead.id, "Old"),
            Err(Refusal::Archived("Old".into()))
        );
        assert_eq!(
            find_child(&all, lead.id, "nope"),
            Err(Refusal::UnknownChild("nope".into()))
        );
    }

    #[test]
    fn names_shared_with_other_workspaces_still_resolve_or_ask_for_the_id() {
        let lead = ws("Lead", None, 0);
        let rival = ws("Rival", None, 0);
        let mine = ws("Docs", Some(&lead), 1);
        let theirs = ws("Docs", Some(&rival), 2);
        let all = [lead.clone(), rival, mine.clone(), theirs];
        assert_eq!(find_child(&all, lead.id, "Docs").map(|w| w.id), Ok(mine.id));
        let twin = ws("Docs", Some(&lead), 3);
        let all = [all.to_vec(), vec![twin]].concat();
        assert_eq!(
            find_child(&all, lead.id, "docs"),
            Err(Refusal::Ambiguous("docs".into()))
        );
    }

    #[test]
    fn a_child_starts_from_the_parents_base_named_after_its_task() {
        let lead = ws("Lead", None, 0);
        let spec = child_workspace(&lead, "\n  Fix the login crash \nwith a test\n", None).unwrap();
        assert_eq!(
            spec,
            NewWorkspace {
                name: "Fix the login crash".into(),
                base: Some("develop".into()),
                branch: None,
                task: Some("Fix the login crash \nwith a test".into()),
                group: None,
                parent: Some(lead.id),
            }
        );
        let named = child_workspace(&lead, "Do it", Some("  Login ")).unwrap();
        assert_eq!(named.name, "Login");
        let blank_name = child_workspace(&lead, "Do it", Some(" ")).unwrap();
        assert_eq!(blank_name.name, "Do it");
        assert_eq!(
            child_workspace(&lead, " \n ", None),
            Err(Refusal::EmptyTask)
        );
        let long = child_workspace(
            &lead,
            "Rewrite the onboarding flow so new users see the permissions screen first",
            None,
        )
        .unwrap();
        assert_eq!(long.name, "Rewrite the onboarding flow so new users…");
    }
}
