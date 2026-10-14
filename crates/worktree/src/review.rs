//! Review actions: stage, commit, push and merge back into the base branch. All through the git
//! CLI, respecting the user's config and hooks (never `--no-verify`, never `--force`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{Git, RawOutput, bytes_to_path};
use crate::repo::{branch_exists, current_branch};
use crate::status::{ChangeKind, status};
use crate::worktree::list;

fn pathspec_args(head: &[&str], paths: &[PathBuf]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--literal-pathspecs".into()];
    args.extend(head.iter().map(OsString::from));
    args.push("--".into());
    args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
    args
}

/// Stages `paths` (worktree-relative), including deletions. An empty list does nothing.
pub fn stage(worktree: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    Git::new(worktree).run_bytes(pathspec_args(&["add", "-A"], paths))?;
    Ok(())
}

/// Unstages `paths`, keeping the working tree as is. An empty list does nothing.
pub fn unstage(worktree: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let git = Git::new(worktree);
    if has_head(&git)? {
        git.run_bytes(pathspec_args(&["restore", "--staged"], paths))?;
    } else {
        // Before the first commit there is no HEAD to restore from.
        git.run_bytes(pathspec_args(
            &["rm", "--cached", "-r", "-q", "--ignore-unmatch"],
            paths,
        ))?;
    }
    Ok(())
}

/// Stages every change, untracked files included (`git add -A`).
pub fn stage_all(worktree: &Path) -> Result<()> {
    Git::new(worktree).run_bytes(["add", "-A"])?;
    Ok(())
}

fn has_head(git: &Git) -> Result<bool> {
    Ok(git
        .query(["rev-parse", "--verify", "--quiet", "HEAD"])?
        .is_some())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitOptions {
    /// `git commit -a`: also commit unstaged changes to tracked files (not untracked ones).
    pub all: bool,
}

/// Commits the index (or all tracked changes with `options.all`) and returns the new HEAD.
/// Hooks and signing follow the user's config; a failing hook is [`Error::HookFailed`].
pub fn commit(worktree: &Path, message: &str, options: CommitOptions) -> Result<String> {
    if message.trim().is_empty() {
        return Err(Error::EmptyMessage);
    }
    let st = status(worktree)?;
    let has_changes = st
        .changes
        .iter()
        .any(|c| c.staged || (options.all && c.unstaged && c.kind != ChangeKind::Untracked));
    if !has_changes {
        return Err(Error::NothingToCommit);
    }
    let git = Git::new(worktree);
    let mut args = vec!["commit", "--quiet", "--file=-"];
    if options.all {
        args.push("--all");
    }
    let out = git.output(&args, Some(message.as_bytes()))?;
    if !out.status.success() {
        return Err(commit_failure(&git, out));
    }
    Ok(git.run(["rev-parse", "HEAD"])?.trim().to_owned())
}

const COMMIT_HOOKS: [&str; 4] = [
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
];

/// git prints nothing of its own when a hook fails, so a failed commit is attributed to hooks
/// when one is installed.
fn commit_failure(git: &Git, out: RawOutput) -> Error {
    let text = format!("{}\n{}", out.stdout_text(), out.stderr_text());
    if text.contains("nothing to commit") || text.contains("no changes added to commit") {
        return Error::NothingToCommit;
    }
    if text.contains("empty commit message") {
        return Error::EmptyMessage;
    }
    let hooks = git
        .run(["rev-parse", "--path-format=absolute", "--git-path", "hooks"])
        .map(|s| PathBuf::from(s.trim()));
    if let Ok(dir) = hooks
        && COMMIT_HOOKS.iter().any(|h| is_executable(&dir.join(h)))
    {
        let stderr = out.stderr_text();
        let stderr = if stderr.is_empty() {
            out.stdout_text().trim().to_owned()
        } else {
            stderr
        };
        return Error::HookFailed { stderr };
    }
    git.failure(out)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Names of the repository's remotes.
pub fn remotes(repo: &Path) -> Result<Vec<String>> {
    let out = Git::new(repo).run(["remote"])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The branch's upstream and how far the branch is from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    /// Short name, e.g. `origin/hangar/fix-login`.
    pub name: String,
    pub remote: String,
    /// Full ref on the remote, e.g. `refs/heads/hangar/fix-login`.
    pub remote_ref: String,
    pub ahead: u32,
    pub behind: u32,
    /// The upstream branch was deleted on the remote (after a fetch).
    pub gone: bool,
}

/// Upstream of the checked-out branch; `None` when detached, when none is set, or when it
/// tracks a differently named branch (e.g. the base it was started from): only a same-named
/// branch on the remote is where [`push`] goes.
pub fn upstream(worktree: &Path) -> Result<Option<Upstream>> {
    let Some(branch) = current_branch(worktree)? else {
        return Ok(None);
    };
    branch_upstream(worktree, &branch)
}

fn branch_upstream(worktree: &Path, branch: &str) -> Result<Option<Upstream>> {
    let git = Git::new(worktree);
    let out = git.run([
        "for-each-ref",
        "--format=%(upstream:short)%00%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream:track)",
        &format!("refs/heads/{branch}"),
    ])?;
    let line = out.lines().next().unwrap_or("");
    let mut parts = line.split('\0');
    let (Some(name), Some(remote), Some(remote_ref)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(None);
    };
    // A branch started from `origin/main` may track it; pushing "to the upstream" would then
    // push the workspace onto the base. Only a same-named remote branch counts.
    if name.is_empty() || remote.is_empty() || remote_ref != format!("refs/heads/{branch}") {
        return Ok(None);
    }
    let gone = parts.next().is_some_and(|t| t.contains("gone"));
    let (ahead, behind) = if gone {
        (0, 0)
    } else {
        let counts = git.run([
            "rev-list",
            "--left-right",
            "--count",
            &format!("refs/heads/{branch}...{name}"),
        ])?;
        parse_counts(&counts)?
    };
    Ok(Some(Upstream {
        name: name.to_owned(),
        remote: remote.to_owned(),
        remote_ref: remote_ref.to_owned(),
        ahead,
        behind,
        gone,
    }))
}

fn parse_counts(out: &str) -> Result<(u32, u32)> {
    let mut it = out.split_whitespace().map(str::parse::<u32>);
    match (it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b))) => Ok((a, b)),
        _ => Err(Error::Parse(format!(
            "rev-list --left-right --count: {out:?}"
        ))),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushOutcome {
    pub remote: String,
    pub branch: String,
    /// This push set the branch's upstream (first push).
    pub set_upstream: bool,
    /// The remote already had this commit.
    pub up_to_date: bool,
}

/// Pushes the checked-out branch to the same-named branch on the remote. With such an
/// upstream it pushes there; otherwise to `remote` (default `origin`, or the only remote)
/// with `-u`, replacing any upstream that pointed at another branch. Never forces.
pub fn push(worktree: &Path, remote: Option<&str>) -> Result<PushOutcome> {
    let branch = current_branch(worktree)?.ok_or(Error::DetachedHead)?;
    let git = Git::new(worktree);
    let upstream = branch_upstream(worktree, &branch)?.filter(|u| !u.gone);
    let (target, refspec, set_upstream) = match (&upstream, remote) {
        (Some(up), None) => (
            up.remote.clone(),
            format!("refs/heads/{branch}:{}", up.remote_ref),
            false,
        ),
        (Some(up), Some(r)) if up.remote == r => (
            up.remote.clone(),
            format!("refs/heads/{branch}:{}", up.remote_ref),
            false,
        ),
        (Some(_), Some(r)) => (
            r.to_owned(),
            format!("refs/heads/{branch}:refs/heads/{branch}"),
            false,
        ),
        (None, Some(r)) => (
            r.to_owned(),
            format!("refs/heads/{branch}:refs/heads/{branch}"),
            true,
        ),
        (None, None) => {
            let all = remotes(worktree)?;
            let r = if all.iter().any(|r| r == "origin") {
                "origin".to_owned()
            } else if let [only] = all.as_slice() {
                only.clone()
            } else {
                return Err(Error::NoRemote);
            };
            (r, format!("refs/heads/{branch}:refs/heads/{branch}"), true)
        }
    };
    let mut args = vec!["push", "--porcelain"];
    if set_upstream {
        args.push("--set-upstream");
    }
    args.extend(["--", target.as_str(), refspec.as_str()]);
    let out = git.output(&args, None)?;
    let report = parse_push_porcelain(&out.stdout_text());
    if let Some(rejected) = report.iter().find(|r| r.flag == '!') {
        let reason = rejected.summary.clone();
        let non_fast_forward =
            reason.contains("non-fast-forward") || reason.contains("fetch first");
        return Err(Error::PushRejected {
            remote: target,
            branch,
            reason,
            non_fast_forward,
        });
    }
    if !out.status.success() {
        return Err(git.failure(out));
    }
    Ok(PushOutcome {
        remote: target,
        branch,
        set_upstream,
        up_to_date: !report.is_empty() && report.iter().all(|r| r.flag == '='),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PushRef {
    pub flag: char,
    pub refspec: String,
    pub summary: String,
}

/// Parses the ref lines of `git push --porcelain`: `<flag>\t<from>:<to>\t<summary> (<reason>)`.
pub(crate) fn parse_push_porcelain(out: &str) -> Vec<PushRef> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let flag = parts.next()?;
            let refspec = parts.next()?;
            let summary = parts.next().unwrap_or("");
            let mut chars = flag.chars();
            let (Some(flag), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if !" +-*=!".contains(flag) {
                return None;
            }
            let reason = summary
                .rsplit_once('(')
                .and_then(|(_, r)| r.strip_suffix(')'))
                .map(str::to_owned)
                .unwrap_or_else(|| summary.trim().to_owned());
            Some(PushRef {
                flag,
                refspec: refspec.to_owned(),
                summary: reason,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStrategy {
    /// `git merge --no-ff`: a merge commit keeps the branch's history.
    NoFf,
    /// `git merge --squash` then one commit.
    Squash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeOutcome {
    /// Checkout where `base` is checked out and the merge was made.
    pub worktree: PathBuf,
    /// The new commit on `base`.
    pub head: String,
}

/// Where `base` is checked out, if it can take a merge now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseCheckout {
    pub path: PathBuf,
    pub is_main: bool,
}

/// Finds the clean checkout of local branch `base` (main or linked worktree). Never checks
/// anything out: a missing or busy checkout is [`Error::BaseNotReady`].
pub fn base_checkout(repo: &Path, base: &str) -> Result<BaseCheckout> {
    if !branch_exists(repo, base)? {
        return Err(Error::BaseNotReady {
            reason: format!("there is no local branch `{base}`"),
        });
    }
    let wt = list(repo)?
        .into_iter()
        .find(|wt| !wt.bare && !wt.prunable && wt.branch.as_deref() == Some(base))
        .ok_or_else(|| Error::BaseNotReady {
            reason: format!(
                "`{base}` is not checked out in any worktree; check it out in the main \
                 repository first"
            ),
        })?;
    let git = Git::new(&wt.path);
    for (file, what) in [
        ("MERGE_HEAD", "a merge"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase"),
    ] {
        let path = git.run(["rev-parse", "--path-format=absolute", "--git-path", file])?;
        if Path::new(path.trim()).exists() {
            return Err(Error::BaseNotReady {
                reason: format!("{} is in the middle of {what}", wt.path.display()),
            });
        }
    }
    let st = status(&wt.path)?;
    if !st.is_clean() {
        let tracked = st.changes.len() - st.untracked;
        return Err(Error::BaseNotReady {
            reason: format!(
                "{} has {tracked} uncommitted and {} untracked file(s); commit or stash them first",
                wt.path.display(),
                st.untracked
            ),
        });
    }
    Ok(BaseCheckout {
        path: wt.path,
        is_main: wt.is_main,
    })
}

/// Paths that merging `branch` into `base` would conflict in (empty = merges cleanly).
/// Uses `git merge-tree --write-tree`, which touches no checkout.
pub fn merge_conflicts(repo: &Path, base: &str, branch: &str) -> Result<Vec<PathBuf>> {
    let git = Git::new(repo);
    let out = git.output(
        [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            &format!("refs/heads/{base}"),
            &format!("refs/heads/{branch}"),
        ],
        None,
    )?;
    match out.status.code() {
        Some(0) => Ok(Vec::new()),
        Some(1) => Ok(parse_merge_tree_conflicts(&out.stdout)),
        _ => Err(git.failure(out)),
    }
}

/// `merge-tree --write-tree --name-only -z` output: `<tree>\0<path>\0...\0\0[messages]`.
fn parse_merge_tree_conflicts(out: &[u8]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = out
        .split(|&b| b == 0)
        .skip(1)
        .take_while(|f| !f.is_empty())
        .map(bytes_to_path)
        .collect();
    paths.dedup();
    paths
}

/// Merges local `branch` into local `base` where `base` is checked out. Refuses (leaving
/// everything untouched) when that checkout is missing or not clean, when the branch is
/// already merged, or when the merge would conflict. If git fails half-way (e.g. a hook), the
/// merge is rolled back so the base checkout is left as it was.
pub fn merge_into_base(
    repo: &Path,
    branch: &str,
    base: &str,
    strategy: MergeStrategy,
    message: &str,
) -> Result<MergeOutcome> {
    if message.trim().is_empty() {
        return Err(Error::EmptyMessage);
    }
    if !branch_exists(repo, branch)? {
        return Err(Error::BranchMissing(branch.to_owned()));
    }
    let checkout = base_checkout(repo, base)?;
    let git = Git::new(&checkout.path);
    let merged = git
        .query([
            "merge-base",
            "--is-ancestor",
            &format!("refs/heads/{branch}"),
            &format!("refs/heads/{base}"),
        ])?
        .is_some();
    if merged {
        return Err(Error::AlreadyMerged {
            branch: branch.to_owned(),
            base: base.to_owned(),
        });
    }
    let paths = merge_conflicts(repo, base, branch)?;
    if !paths.is_empty() {
        return Err(Error::MergeConflicts {
            branch: branch.to_owned(),
            base: base.to_owned(),
            paths,
        });
    }
    let branch_ref = format!("refs/heads/{branch}");
    match strategy {
        MergeStrategy::NoFf => {
            let out = git.output(
                [
                    "merge",
                    "--no-ff",
                    "--no-edit",
                    "--quiet",
                    "-m",
                    message,
                    &branch_ref,
                ],
                None,
            );
            match out {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let err = commit_failure(&git, out);
                    return Err(rollback_merge(&git, &checkout, err, true));
                }
                Err(err) => return Err(rollback_merge(&git, &checkout, err, true)),
            }
        }
        MergeStrategy::Squash => {
            let squash = git.output(["merge", "--squash", "--quiet", &branch_ref], None);
            match squash {
                Ok(out) if out.status.success() => {}
                Ok(out) => return Err(rollback_merge(&git, &checkout, git.failure(out), false)),
                Err(err) => return Err(rollback_merge(&git, &checkout, err, false)),
            }
            let out = git.output(["commit", "--quiet", "--file=-"], Some(message.as_bytes()));
            match out {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let err = commit_failure(&git, out);
                    return Err(rollback_merge(&git, &checkout, err, false));
                }
                Err(err) => return Err(rollback_merge(&git, &checkout, err, false)),
            }
        }
    }
    let head = git.run(["rev-parse", "HEAD"])?.trim().to_owned();
    Ok(MergeOutcome {
        worktree: checkout.path,
        head,
    })
}

/// Undoes a merge that git started but did not finish. Safe because the checkout was verified
/// clean before merging: only the merge's own changes are reset.
fn rollback_merge(git: &Git, checkout: &BaseCheckout, err: Error, no_ff: bool) -> Error {
    let merge_head = git
        .run([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "MERGE_HEAD",
        ])
        .is_ok_and(|p| Path::new(p.trim()).exists());
    let undo: &[&str] = if no_ff && merge_head {
        &["merge", "--abort"]
    } else {
        &["reset", "--merge"]
    };
    if let Err(undo_err) = git.run(undo) {
        tracing::error!(
            path = %checkout.path.display(),
            %undo_err,
            "failed to roll back an unfinished merge"
        );
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_push_porcelain() {
        let out = "To /tmp/origin.git\n\
=\trefs/heads/a:refs/heads/a\t[up to date]\n\
!\trefs/heads/b:refs/heads/b\t[rejected] (non-fast-forward)\n\
*\trefs/heads/c:refs/heads/c\t[new branch]\n\
Done\n";
        let refs = parse_push_porcelain(out);
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].flag, '=');
        assert_eq!(refs[1].flag, '!');
        assert_eq!(refs[1].summary, "non-fast-forward");
        assert_eq!(refs[1].refspec, "refs/heads/b:refs/heads/b");
        assert_eq!(refs[2].summary, "[new branch]");
    }

    #[test]
    fn parses_merge_tree_output() {
        let out = b"abcd\0a.txt\0dir/b c.txt\0\0";
        assert_eq!(
            parse_merge_tree_conflicts(out),
            [PathBuf::from("a.txt"), PathBuf::from("dir/b c.txt")]
        );
        assert!(parse_merge_tree_conflicts(b"abcd\0").is_empty());
        assert_eq!(parse_counts("2\t3\n").unwrap(), (2, 3));
        assert!(parse_counts("x").is_err());
    }
}
