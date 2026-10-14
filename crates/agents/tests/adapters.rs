#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hangar_agents::{
    AgentAdapter, Error, GridSize, PtyAdapter, SpawnContext, find_program, presets,
};
use hangar_worktree::CreateOptions;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn finds_programs_on_a_given_path() {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin dir");
    fs::create_dir(&bin).unwrap();
    let tool = bin.join("fake-agent");
    fs::write(&tool, "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let plain = bin.join("not-executable");
    fs::write(&plain, "data").unwrap();
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();

    let path_var = std::env::join_paths(["/nonexistent-dir".as_ref(), bin.as_path()]).unwrap();
    assert_eq!(
        find_program("fake-agent", Some(&path_var)),
        Some(tool.clone())
    );
    assert_eq!(find_program("not-executable", Some(&path_var)), None);
    assert_eq!(find_program("bin dir", Some(&path_var)), None);
    assert_eq!(find_program("fake-agent", None), None);
    assert_eq!(find_program("", Some(&path_var)), None);
    assert_eq!(find_program(tool.to_str().unwrap(), None), Some(tool));
    assert_eq!(
        find_program("sh", Some("/bin".as_ref())),
        Some("/bin/sh".into())
    );
}

#[test]
fn context_path_is_used_to_resolve_the_program() {
    let tmp = TempDir::new().unwrap();
    let tool = tmp.path().join("hangar-fake-agent");
    fs::write(&tool, "#!/bin/sh\necho fake-agent-ran\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();

    let adapter = PtyAdapter::new("fake", "Fake", "hangar-fake-agent");
    assert!(!adapter.is_installed());
    let mut ctx = SpawnContext::new(tmp.path());
    ctx.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", tmp.path().display()),
    ));
    let handle = adapter.spawn(&ctx).unwrap();
    assert_eq!(handle.program(), tool);
    let exit = handle
        .session()
        .wait_for_exit(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert!(exit.success());
    assert!(screen_text(&handle).contains("fake-agent-ran"));
}

#[test]
fn spawns_in_a_worktree_with_env() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Hangar Test"]);
    git(&repo, &["config", "user.email", "test@hangar.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "hi\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--no-verify", "-q", "-m", "initial"]);
    let wt = hangar_worktree::create(
        &repo,
        &CreateOptions {
            branch: "task".into(),
            base: None,
            path: root.join("wt task"),
            reuse_existing_branch: false,
        },
    )
    .unwrap();

    let adapter = PtyAdapter::new("sh", "Shell test", "/bin/sh")
        .with_args(["-c"])
        .with_env("HANGAR_TEST", "from-adapter");
    let updates = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&updates);
    let mut ctx = SpawnContext::new(&wt.path);
    ctx.size = GridSize::new(400, 24).unwrap();
    ctx.extra_args
        .push("pwd; echo \"value=$HANGAR_TEST\"".into());
    ctx.env.push(("HANGAR_TEST".into(), "from-context".into()));
    ctx.on_update = Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    }));

    let handle = adapter.spawn(&ctx).unwrap();
    assert_eq!(handle.agent_id(), "sh");
    let exit = handle
        .session()
        .wait_for_exit(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert!(exit.success(), "{exit:?}");
    let text = screen_text(&handle);
    assert!(text.contains(wt.path.to_str().unwrap()), "{text}");
    assert!(text.contains("value=from-context"), "{text}");
    assert!(updates.load(Ordering::Relaxed) >= 1);
}

#[test]
fn unknown_program_is_explicit() {
    let adapter = PtyAdapter::new("ghost", "Ghost", "hangar-no-such-agent-xyz");
    assert!(!adapter.is_installed());
    let tmp = TempDir::new().unwrap();
    match adapter.spawn(&SpawnContext::new(tmp.path())) {
        Err(Error::NotInstalled { agent, program }) => {
            assert_eq!(agent, "ghost");
            assert_eq!(program, "hangar-no-such-agent-xyz");
        }
        other => panic!("expected NotInstalled, got {other:?}"),
    }

    let sh = PtyAdapter::new("sh", "sh", "/bin/sh");
    assert!(sh.is_installed());
    let missing = tmp.path().join("missing");
    assert!(matches!(
        sh.spawn(&SpawnContext::new(&missing)),
        Err(Error::InvalidCwd(path)) if path == missing
    ));
}

#[test]
fn presets_cover_the_supported_agents() {
    let presets = presets();
    let ids: Vec<_> = presets.iter().map(|p| p.id()).collect();
    assert_eq!(ids, ["claude", "codex", "opencode", "gemini", "shell"]);
    for preset in &presets {
        assert!(!preset.display_name().is_empty());
        assert!(preset.env.is_empty());
        if preset.id != "shell" {
            assert!(preset.args.is_empty(), "{preset:?}");
        }
    }
    let shell = presets.iter().find(|p| p.id == "shell").unwrap();
    assert_eq!(shell.args, ["-l"]);
    let adapters: Vec<Box<dyn AgentAdapter>> = presets
        .into_iter()
        .map(|p| Box::new(p) as Box<dyn AgentAdapter>)
        .collect();
    assert_eq!(adapters.len(), 5);
}

fn screen_text(handle: &hangar_agents::AgentHandle) -> String {
    let snapshot = handle.session().terminal().snapshot();
    (0..snapshot.rows)
        .map(|r| snapshot.row_text(r) + "\n")
        .collect()
}
