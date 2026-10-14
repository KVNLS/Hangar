//! First-run setup: what a user needs outside Hangar (git, an agent, `gh`, Xcode, the Android SDK,
//! a JDK, Node) and what to run when something is missing. Hangar only shows the commands; it
//! never installs anything itself.

use std::path::PathBuf;

use crate::review::GhStatus;

/// What [`crate::ports::ToolProbe`] found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolFacts {
    /// Why the login shell's `PATH` could not be read; `None` when it was.
    pub path_error: Option<String>,
    pub homebrew: bool,
    pub git: GitFact,
    pub gh: GhStatus,
    /// Agent CLIs and the ACP adapters that drive them.
    pub agents: Vec<AgentFact>,
    pub xcode: XcodeFact,
    pub android: AndroidFact,
    /// `JAVA_HOME` of the default JDK.
    pub jdk: Option<PathBuf>,
    pub node: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitFact {
    Missing,
    /// Older than the minimum Hangar needs.
    TooOld {
        version: String,
        required: String,
    },
    Installed {
        version: String,
        name: Option<String>,
        email: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentFact {
    pub id: String,
    pub name: String,
    pub program: String,
    pub path: Option<PathBuf>,
    pub install_hint: String,
    /// For an ACP adapter: the id of the CLI it drives.
    pub drives: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XcodeFact {
    Missing,
    /// `xcode-select` points at the Command Line Tools, which have no simulator.
    CommandLineToolsOnly,
    Installed {
        /// The Xcode app bundle, e.g. `/Applications/Xcode.app`.
        app: PathBuf,
        /// The license is accepted and the first-launch packages are installed.
        first_launch_done: bool,
        ios_runtimes: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AndroidFact {
    Missing,
    Installed { sdk: PathBuf, avds: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SetupGroup {
    Essentials,
    Agents,
    GitHub,
    Ios,
    Android,
    Run,
}

impl SetupGroup {
    pub const ALL: [SetupGroup; 6] = [
        Self::Essentials,
        Self::Agents,
        Self::GitHub,
        Self::Ios,
        Self::Android,
        Self::Run,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Essentials => "Essentials",
            Self::Agents => "Agents",
            Self::GitHub => "GitHub",
            Self::Ios => "iOS simulator",
            Self::Android => "Android emulator",
            Self::Run => "Build and run",
        }
    }

    /// When the group is needed, for groups that are optional as a whole.
    pub fn needed_for(self) -> Option<&'static str> {
        match self {
            Self::Essentials | Self::Agents => None,
            Self::GitHub => Some("Needed to open pull requests."),
            Self::Ios => Some("Needed for iOS projects."),
            Self::Android => Some("Needed for Android projects."),
            Self::Run => Some("Needed to build React Native, Expo and Gradle projects."),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckState {
    Ok,
    /// Not installed.
    Missing,
    /// Installed but not ready (not signed in, not configured...).
    NeedsAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupCheck {
    /// Stable key, e.g. `git`, `gh-auth`, `agent:claude`.
    pub id: String,
    pub group: SetupGroup,
    pub title: String,
    pub state: CheckState,
    /// What was found (a version or path) when it is fine; what is wrong otherwise.
    pub detail: String,
    /// A command to run in a terminal, when one fixes it.
    pub fix: Option<String>,
    /// Hangar cannot do its job without it.
    pub required: bool,
}

impl SetupCheck {
    pub fn is_ok(&self) -> bool {
        self.state == CheckState::Ok
    }
}

pub const BREW_INSTALL: &str = "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"";
pub const GIT_IDENTITY: &str =
    "git config --global user.name \"Your Name\" && git config --global user.email you@example.com";
pub const XCODE_APP_STORE: &str = "open \"macappstore://apps.apple.com/app/id497799835\"";
pub const XCODE_SELECT: &str = "sudo xcode-select -s /Applications/Xcode.app";
pub const XCODE_FIRST_LAUNCH: &str = "sudo xcodebuild -runFirstLaunch";
pub const IOS_RUNTIME: &str = "xcodebuild -downloadPlatform iOS";
pub const ANDROID_STUDIO: &str = "brew install --cask android-studio";
pub const JDK: &str = "brew install --cask zulu@17";
pub const NODE: &str = "brew install node";
pub const GH: &str = "brew install gh";
pub const GH_LOGIN: &str = "gh auth login";

/// Every check, grouped in [`SetupGroup::ALL`] order. Fine items are kept so a settings page can
/// list what was found.
pub fn checklist(facts: &ToolFacts) -> Vec<SetupCheck> {
    let mut out = Vec::new();
    essentials(facts, &mut out);
    agents(facts, &mut out);
    github(facts, &mut out);
    ios(facts, &mut out);
    android(facts, &mut out);
    run(facts, &mut out);
    out
}

fn check(
    id: impl Into<String>,
    group: SetupGroup,
    title: impl Into<String>,
    state: CheckState,
    detail: impl Into<String>,
    fix: Option<&str>,
) -> SetupCheck {
    SetupCheck {
        id: id.into(),
        group,
        title: title.into(),
        state,
        detail: detail.into(),
        fix: fix.map(str::to_owned),
        required: false,
    }
}

fn required(mut c: SetupCheck) -> SetupCheck {
    c.required = true;
    c
}

fn path_detail(path: &std::path::Path) -> String {
    path.display().to_string()
}

fn essentials(facts: &ToolFacts, out: &mut Vec<SetupCheck>) {
    use CheckState::*;
    use SetupGroup::Essentials as G;
    if let Some(reason) = &facts.path_error {
        out.push(check(
            "path",
            G,
            "Login shell PATH",
            NeedsAction,
            format!(
                "Hangar could not read your shell's PATH, so tools installed with Homebrew, npm \
                 or mise may look missing. Check that your shell profile runs without prompts. \
                 ({reason})"
            ),
            None,
        ));
    }
    if !facts.homebrew {
        out.push(check(
            "homebrew",
            G,
            "Homebrew",
            Missing,
            "Most of the commands below install tools with Homebrew.",
            Some(BREW_INSTALL),
        ));
    }
    out.push(required(match &facts.git {
        GitFact::Missing => check(
            "git",
            G,
            "Git",
            Missing,
            "Every workspace is a Git worktree.",
            Some("xcode-select --install"),
        ),
        GitFact::TooOld { version, required } => check(
            "git",
            G,
            "Git",
            NeedsAction,
            format!("Git {version} is too old; Hangar needs {required} or newer."),
            Some("brew install git"),
        ),
        GitFact::Installed { version, .. } => check("git", G, "Git", Ok, version.clone(), None),
    }));
    if let GitFact::Installed { name, email, .. } = &facts.git {
        out.push(required(match (name, email) {
            (Some(name), Some(email)) => check(
                "git-identity",
                G,
                "Git identity",
                Ok,
                format!("{name} <{email}>"),
                None,
            ),
            _ => check(
                "git-identity",
                G,
                "Git identity",
                NeedsAction,
                "Commits fail until Git knows your name and email.",
                Some(GIT_IDENTITY),
            ),
        }));
    }
}

fn agents(facts: &ToolFacts, out: &mut Vec<SetupCheck>) {
    use CheckState::*;
    use SetupGroup::Agents as G;
    let clis: Vec<&AgentFact> = facts.agents.iter().filter(|a| a.drives.is_none()).collect();
    if !clis.iter().any(|a| a.path.is_some()) {
        let first = clis.iter().find(|a| !a.install_hint.is_empty());
        out.push(required(check(
            "agent",
            G,
            "A coding agent",
            Missing,
            match first {
                Some(a) => format!("Install at least one agent CLI, for example {}.", a.name),
                None => "Install at least one agent CLI.".to_owned(),
            },
            first.map(|a| a.install_hint.as_str()),
        )));
    }
    for agent in &facts.agents {
        let cli_found = |id: &str| facts.agents.iter().any(|a| a.id == id && a.path.is_some());
        let same_program = |id: &str| {
            facts
                .agents
                .iter()
                .any(|a| a.id == id && a.program == agent.program)
        };
        match &agent.drives {
            // An adapter that is the CLI itself (`opencode acp`) needs nothing more; one for a
            // CLI that is not installed is noise.
            Some(cli) if same_program(cli) || !cli_found(cli) => continue,
            _ => {}
        }
        let id = format!("agent:{}", agent.id);
        out.push(match &agent.path {
            Some(path) => check(id, G, &agent.name, Ok, path_detail(path), None),
            None => check(
                id,
                G,
                &agent.name,
                Missing,
                match agent.drives {
                    Some(_) => format!("`{}` runs this agent in a chat pane.", agent.program),
                    None => format!("`{}` is not on PATH.", agent.program),
                },
                (!agent.install_hint.is_empty()).then_some(agent.install_hint.as_str()),
            ),
        });
    }
}

fn github(facts: &ToolFacts, out: &mut Vec<SetupCheck>) {
    use CheckState::*;
    use SetupGroup::GitHub as G;
    match &facts.gh {
        GhStatus::Missing => out.push(check(
            "gh",
            G,
            "GitHub CLI",
            Missing,
            "Pull requests are opened with `gh`.",
            Some(GH),
        )),
        GhStatus::Installed {
            authenticated,
            account,
        } => {
            out.push(check("gh", G, "GitHub CLI", Ok, "installed", None));
            out.push(if *authenticated {
                check(
                    "gh-auth",
                    G,
                    "GitHub sign-in",
                    Ok,
                    account.clone().unwrap_or_else(|| "signed in".to_owned()),
                    None,
                )
            } else {
                check(
                    "gh-auth",
                    G,
                    "GitHub sign-in",
                    NeedsAction,
                    "Sign in once so Hangar can open pull requests.",
                    Some(GH_LOGIN),
                )
            });
        }
    }
}

fn ios(facts: &ToolFacts, out: &mut Vec<SetupCheck>) {
    use CheckState::*;
    use SetupGroup::Ios as G;
    match &facts.xcode {
        XcodeFact::Missing => out.push(check(
            "xcode",
            G,
            "Xcode",
            Missing,
            "Install Xcode from the App Store, then open it once.",
            Some(XCODE_APP_STORE),
        )),
        XcodeFact::CommandLineToolsOnly => out.push(check(
            "xcode",
            G,
            "Xcode",
            NeedsAction,
            "The Command Line Tools are selected; simulators need Xcode itself.",
            Some(XCODE_SELECT),
        )),
        XcodeFact::Installed {
            app,
            first_launch_done,
            ios_runtimes,
        } => {
            out.push(check("xcode", G, "Xcode", Ok, path_detail(app), None));
            out.push(if *first_launch_done {
                check("xcode-setup", G, "Xcode first launch", Ok, "done", None)
            } else {
                check(
                    "xcode-setup",
                    G,
                    "Xcode first launch",
                    NeedsAction,
                    "Accept the license and install Xcode's extra components.",
                    Some(XCODE_FIRST_LAUNCH),
                )
            });
            out.push(if *ios_runtimes > 0 {
                check(
                    "ios-runtime",
                    G,
                    "iOS simulator runtime",
                    Ok,
                    plural(*ios_runtimes, "runtime"),
                    None,
                )
            } else {
                check(
                    "ios-runtime",
                    G,
                    "iOS simulator runtime",
                    Missing,
                    "No iOS simulator runtime is installed.",
                    Some(IOS_RUNTIME),
                )
            });
        }
    }
}

fn android(facts: &ToolFacts, out: &mut Vec<SetupCheck>) {
    use CheckState::*;
    use SetupGroup::Android as G;
    match &facts.android {
        AndroidFact::Missing => out.push(check(
            "android-sdk",
            G,
            "Android SDK",
            Missing,
            "Install Android Studio, or set ANDROID_HOME to an existing SDK.",
            Some(ANDROID_STUDIO),
        )),
        AndroidFact::Installed { sdk, avds } => {
            out.push(check(
                "android-sdk",
                G,
                "Android SDK",
                Ok,
                path_detail(sdk),
                None,
            ));
            out.push(if *avds > 0 {
                check(
                    "avd",
                    G,
                    "Emulator",
                    Ok,
                    plural(*avds, "virtual device"),
                    None,
                )
            } else {
                check(
                    "avd",
                    G,
                    "Emulator",
                    Missing,
                    "Create a virtual device in Android Studio › Device Manager.",
                    None,
                )
            });
        }
    }
}

fn run(facts: &ToolFacts, out: &mut Vec<SetupCheck>) {
    use CheckState::*;
    use SetupGroup::Run as G;
    out.push(match &facts.node {
        Some(path) => check("node", G, "Node.js", Ok, path_detail(path), None),
        None => check(
            "node",
            G,
            "Node.js",
            Missing,
            "React Native and Expo projects run with `npx`.",
            Some(NODE),
        ),
    });
    out.push(match &facts.jdk {
        Some(path) => check("jdk", G, "Java (JDK)", Ok, path_detail(path), None),
        None => check(
            "jdk",
            G,
            "Java (JDK)",
            Missing,
            "Gradle needs a JDK to build Android apps.",
            Some(JDK),
        ),
    });
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

#[cfg(test)]
mod tests;
