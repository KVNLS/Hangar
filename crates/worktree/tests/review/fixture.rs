use super::*;

pub(super) fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

pub(super) fn configure(dir: &Path, hooks: &Path) {
    git(dir, &["config", "user.name", "Hangar Test"]);
    git(dir, &["config", "user.email", "test@hangar.invalid"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    // Isolate from any global hooks directory.
    git(dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
}

/// `repo` on `main` (pushed to a bare `origin`) plus a linked worktree on `hangar/task`.
pub(super) struct Fixture {
    _tmp: TempDir,
    pub(super) root: PathBuf,
    pub(super) repo: PathBuf,
    pub(super) origin: PathBuf,
    pub(super) wt: PathBuf,
    pub(super) hooks: PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let hooks = root.join("hooks");
        fs::create_dir(&hooks).unwrap();
        let origin = root.join("origin.git");
        git(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        configure(&origin, &hooks);
        let repo = root.join("my repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        configure(&repo, &hooks);
        fs::write(repo.join("README.md"), "one\ntwo\nthree\n").unwrap();
        fs::write(repo.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        let wt = root.join("worktrees").join("task");
        create(
            &repo,
            &CreateOptions {
                branch: "hangar/task".into(),
                base: Some("main".into()),
                path: wt.clone(),
                reuse_existing_branch: false,
            },
        )
        .unwrap();
        Self {
            _tmp: tmp,
            root,
            repo,
            origin,
            wt,
            hooks,
        }
    }

    pub(super) fn write(&self, dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, text).unwrap();
    }

    pub(super) fn hook(&self, name: &str, script: &str) {
        let path = self.hooks.join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    pub(super) fn head(&self, dir: &Path) -> String {
        git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Commits `path` on the workspace branch through the API.
    pub(super) fn commit_in_wt(&self, path: &str, text: &str, message: &str) -> String {
        self.write(&self.wt, path, text);
        stage(&self.wt, &[PathBuf::from(path)]).unwrap();
        commit(&self.wt, message, CommitOptions::default()).unwrap()
    }
}

pub(super) fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}
