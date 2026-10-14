use super::*;

/// A fake `gh` that records its arguments and the env it got, one line each, then `--`.
pub(super) struct FakeGh {
    _tmp: TempDir,
    dir: PathBuf,
    log: PathBuf,
}

impl FakeGh {
    fn new(logged_in: bool) -> Self {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let log = dir.join("args.log");
        let auth = if logged_in {
            "echo 'github.com'; echo '  ✓ Logged in to github.com account fake-user (keyring)'; echo '  - Token: gho_************'; exit 0"
        } else {
            "echo 'You are not logged into any GitHub hosts. To log in, run: gh auth login' >&2; exit 1"
        };
        let script = format!(
            r#"#!/bin/sh
for a in "$@"; do printf '%s\n' "$a" >> '{log}'; done
echo "env GH_PROMPT_DISABLED=$GH_PROMPT_DISABLED NO_COLOR=$NO_COLOR" >> '{log}'
echo "cwd $(pwd -P)" >> '{log}'
echo -- >> '{log}'
case "$1 $2" in
  "auth status") {auth} ;;
  "pr view")
    if [ "$3" = "hangar/none" ]; then echo "no pull requests found for branch \"$3\"" >&2; exit 1; fi
    if [ "$3" = "hangar/broken" ]; then echo "HTTP 502" >&2; exit 1; fi
    echo '{{"number":7,"state":"OPEN","title":"Fix login","url":"https://github.com/o/r/pull/7"}}' ;;
  "pr create") echo 'Creating pull request for hangar/task into main in o/r'; echo; echo 'https://github.com/o/r/pull/8' ;;
  *) echo "unexpected" >&2; exit 2 ;;
esac
"#,
            log = log.display()
        );
        let gh = dir.join("gh");
        fs::write(&gh, script).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _tmp: tmp,
            dir,
            log,
        }
    }

    fn gh(&self) -> Gh {
        Gh::new().with_path_env(format!("{}:/usr/bin:/bin", self.dir.display()))
    }

    fn calls(&self) -> Vec<Vec<String>> {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        text.split("--\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().map(str::to_owned).collect())
            .collect()
    }
}

#[test]
fn gh_status_view_and_create() {
    let fake = FakeGh::new(true);
    let gh = fake.gh();
    assert_eq!(
        gh.status().unwrap(),
        GhStatus::Installed {
            authenticated: true,
            account: Some("fake-user".into())
        }
    );
    let fx = Fixture::new();
    assert_eq!(
        gh.pr_for_branch(&fx.wt, "hangar/task").unwrap(),
        Some(PrInfo {
            number: 7,
            url: "https://github.com/o/r/pull/7".into(),
            state: "OPEN".into(),
            title: "Fix login".into(),
        })
    );
    assert_eq!(gh.pr_for_branch(&fx.wt, "hangar/none").unwrap(), None);
    assert!(matches!(
        gh.pr_for_branch(&fx.wt, "hangar/broken"),
        Err(Error::Gh { .. })
    ));
    let url = gh
        .create_pr(
            &fx.wt,
            &CreatePr {
                base: "main".into(),
                head: "hangar/task".into(),
                title: "Fix login".into(),
                body: "Line 1\nLine 2".into(),
                draft: true,
            },
        )
        .unwrap();
    assert_eq!(url, "https://github.com/o/r/pull/8");

    let calls = fake.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0][..2], ["auth", "status"]);
    assert!(
        calls
            .iter()
            .all(|c| c.contains(&"env GH_PROMPT_DISABLED=1 NO_COLOR=1".to_owned()))
    );
    assert_eq!(
        calls[1][..5],
        [
            "pr",
            "view",
            "hangar/task",
            "--json",
            "number,url,state,title"
        ]
    );
    assert!(calls[1].contains(&format!("cwd {}", fx.wt.display())));
    assert_eq!(
        calls[4][..12],
        [
            "pr",
            "create",
            "--base",
            "main",
            "--head",
            "hangar/task",
            "--title",
            "Fix login",
            "--body",
            "Line 1",
            "Line 2",
            "--draft"
        ]
    );
}

#[test]
fn gh_unauthenticated_and_missing() {
    let fake = FakeGh::new(false);
    assert_eq!(
        fake.gh().status().unwrap(),
        GhStatus::Installed {
            authenticated: false,
            account: None
        }
    );
    let empty = TempDir::new().unwrap();
    let gh = Gh::new().with_path_env(empty.path());
    assert_eq!(gh.status().unwrap(), GhStatus::Missing);
    assert!(matches!(
        gh.pr_for_branch(empty.path(), "x"),
        Err(Error::GhMissing)
    ));
}
