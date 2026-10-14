use std::path::PathBuf;

use super::*;

fn agent(id: &str, program: &str, found: bool, drives: Option<&str>) -> AgentFact {
    AgentFact {
        id: id.to_owned(),
        name: id.to_owned(),
        program: program.to_owned(),
        path: found.then(|| PathBuf::from(format!("/bin/{program}"))),
        install_hint: format!("install {program}"),
        drives: drives.map(str::to_owned),
    }
}

/// A machine with everything set up.
fn ready() -> ToolFacts {
    ToolFacts {
        path_error: None,
        not_answering: Vec::new(),
        homebrew: true,
        git: GitFact::Installed {
            version: "2.50.1".into(),
            name: Some("Ada".into()),
            email: Some("ada@example.com".into()),
        },
        gh: GhStatus::Installed {
            authenticated: true,
            account: Some("ada".into()),
        },
        agents: vec![
            agent("claude", "claude", true, None),
            agent("claude-acp", "claude-agent-acp", true, Some("claude")),
            agent("opencode", "opencode", false, None),
            agent("opencode-acp", "opencode", false, Some("opencode")),
        ],
        xcode: XcodeFact::Installed {
            app: "/Applications/Xcode.app".into(),
            first_launch_done: true,
            ios_runtimes: 2,
        },
        android: AndroidFact::Installed {
            sdk: "/sdk".into(),
            avds: 1,
        },
        jdk: Some("/jdk".into()),
        node: Some("/bin/node".into()),
    }
}

fn find<'a>(checks: &'a [SetupCheck], id: &str) -> Option<&'a SetupCheck> {
    checks.iter().find(|c| c.id == id)
}

fn failing(facts: &ToolFacts) -> Vec<String> {
    checklist(facts)
        .into_iter()
        .filter(|c| !c.is_ok())
        .map(|c| c.id)
        .collect()
}

#[test]
fn ready_machine_only_misses_optional_agents() {
    assert_eq!(failing(&ready()), ["agent:opencode"]);
}

#[test]
fn checks_come_in_group_order() {
    let groups: Vec<SetupGroup> = checklist(&ready()).iter().map(|c| c.group).collect();
    let mut sorted = groups.clone();
    sorted.sort();
    assert_eq!(groups, sorted);
}

#[test]
fn no_agent_is_a_required_failure_with_an_install_command() {
    let mut facts = ready();
    for a in &mut facts.agents {
        a.path = None;
    }
    let checks = checklist(&facts);
    let agent = find(&checks, "agent").unwrap();
    assert!(agent.required);
    assert_eq!(agent.state, CheckState::Missing);
    assert_eq!(agent.fix.as_deref(), Some("install claude"));
    // Adapters of CLIs that are not installed stay out of the way.
    assert!(find(&checks, "agent:claude-acp").is_none());
}

#[test]
fn adapter_shows_only_for_an_installed_cli_with_a_separate_program() {
    let mut facts = ready();
    facts.agents[1].path = None;
    facts.agents[2].path = Some("/bin/opencode".into());
    let checks = checklist(&facts);
    let adapter = find(&checks, "agent:claude-acp").unwrap();
    assert_eq!(adapter.state, CheckState::Missing);
    assert_eq!(adapter.fix.as_deref(), Some("install claude-agent-acp"));
    assert!(!adapter.required);
    // `opencode acp` is the CLI itself.
    assert!(find(&checks, "agent:opencode-acp").is_none());
}

#[test]
fn git_identity_is_required() {
    let mut facts = ready();
    facts.git = GitFact::Installed {
        version: "2.50.1".into(),
        name: Some("Ada".into()),
        email: None,
    };
    let checks = checklist(&facts);
    let identity = find(&checks, "git-identity").unwrap();
    assert!(identity.required);
    assert_eq!(identity.state, CheckState::NeedsAction);
    assert_eq!(identity.fix.as_deref(), Some(GIT_IDENTITY));
}

#[test]
fn missing_git_hides_the_identity_check() {
    let mut facts = ready();
    facts.git = GitFact::Missing;
    let checks = checklist(&facts);
    assert_eq!(find(&checks, "git").unwrap().state, CheckState::Missing);
    assert!(find(&checks, "git-identity").is_none());
}

#[test]
fn gh_signed_out_asks_for_login() {
    let mut facts = ready();
    facts.gh = GhStatus::Installed {
        authenticated: false,
        account: None,
    };
    let checks = checklist(&facts);
    let auth = find(&checks, "gh-auth").unwrap();
    assert_eq!(auth.state, CheckState::NeedsAction);
    assert_eq!(auth.fix.as_deref(), Some(GH_LOGIN));
    facts.gh = GhStatus::Missing;
    let checks = checklist(&facts);
    assert_eq!(find(&checks, "gh").unwrap().fix.as_deref(), Some(GH));
    assert!(find(&checks, "gh-auth").is_none());
}

#[test]
fn command_line_tools_only_asks_to_select_xcode() {
    let mut facts = ready();
    facts.xcode = XcodeFact::CommandLineToolsOnly;
    let checks = checklist(&facts);
    assert_eq!(
        find(&checks, "xcode").unwrap().fix.as_deref(),
        Some(XCODE_SELECT)
    );
    assert!(find(&checks, "ios-runtime").is_none());
}

#[test]
fn xcode_setup_and_runtimes() {
    let mut facts = ready();
    facts.xcode = XcodeFact::Installed {
        app: "/Applications/Xcode.app".into(),
        first_launch_done: false,
        ios_runtimes: 0,
    };
    assert_eq!(
        failing(&facts),
        ["agent:opencode", "xcode-setup", "ios-runtime"]
    );
}

#[test]
fn android_without_avds() {
    let mut facts = ready();
    facts.android = AndroidFact::Installed {
        sdk: "/sdk".into(),
        avds: 0,
    };
    let checks = checklist(&facts);
    let avd = find(&checks, "avd").unwrap();
    assert_eq!(avd.state, CheckState::Missing);
    assert_eq!(avd.fix, None);
}

#[test]
fn no_homebrew_and_no_login_path_come_first() {
    let mut facts = ready();
    facts.homebrew = false;
    facts.path_error = Some("timed out".into());
    let checks = checklist(&facts);
    assert_eq!(checks[0].id, "path");
    assert!(checks[0].detail.contains("timed out"));
    assert_eq!(checks[1].id, "homebrew");
    assert_eq!(checks[1].fix.as_deref(), Some(BREW_INSTALL));
}

#[test]
fn only_essentials_and_agents_are_required() {
    let mut facts = ready();
    facts.gh = GhStatus::Missing;
    facts.xcode = XcodeFact::Missing;
    facts.android = AndroidFact::Missing;
    facts.jdk = None;
    facts.node = None;
    for c in checklist(&facts) {
        let core = matches!(c.group, SetupGroup::Essentials | SetupGroup::Agents);
        assert!(!c.required || core, "{} should be optional", c.id);
    }
}

#[test]
fn tools_that_do_not_answer_say_so_instead_of_missing() {
    let mut facts = ready();
    facts.gh = GhStatus::Missing;
    facts.xcode = XcodeFact::Missing;
    facts.not_answering = vec!["gh".into(), "xcode-select".into()];
    let checks = checklist(&facts);
    for id in ["gh", "xcode"] {
        let c = find(&checks, id).unwrap();
        assert_eq!(c.state, CheckState::NeedsAction);
        assert!(c.detail.contains("did not answer"), "{}", c.detail);
        assert_eq!(c.fix, None);
    }
    assert!(find(&checks, "gh-auth").is_none());
}
