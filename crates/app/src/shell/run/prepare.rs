//! What a run works out off the main thread, and how its commands are spawned.

use super::*;

/// Everything worked out off the main thread before the tabs start.
pub(super) struct Prepared {
    pub(super) plan: RunPlan,
    pub(super) setup: Option<hangar_runner::Command>,
    pub(super) stack: &'static str,
    /// [`Stack::key`] of what runs, saved so a picked app is picked again.
    pub(super) stack_key: String,
    pub(super) port: u16,
    pub(super) scheme: Option<String>,
    pub(super) target: hangar_runner::Target,
    pub(super) adb: Option<PathBuf>,
    pub(super) env: Vec<(String, String)>,
}

pub(super) fn prepare(
    root: &Path,
    build_dir: &Path,
    mut target: hangar_runner::Target,
    saved: RunConfig,
    port: u16,
    path: Option<String>,
) -> anyhow::Result<Prepared> {
    let stacks = hangar_runner::detect(root);
    let stack = match hangar_runner::choose_stack(&stacks, target.platform, saved.stack.as_deref())
    {
        Some(hangar_runner::Choice::Run(stack)) => stack,
        Some(hangar_runner::Choice::Choose(candidates)) => {
            return Err(NeedsChoice {
                kind: ChoiceKind::Stack,
                options: candidates.iter().map(|s| (s.key(), s.describe())).collect(),
            }
            .into());
        }
        None => anyhow::bail!(
            "no app to run on {} in {} (looked for Expo, React Native, Xcode and Gradle projects)",
            target.platform.label(),
            root.display()
        ),
    };
    hangar_runner::check(stack, root)?;
    let scheme = match stack {
        Stack::Xcode { container } => {
            let schemes = hangar_runner::xcode_schemes(root, container)?;
            match hangar_runner::pick_scheme(container, &schemes, saved.scheme.as_deref()) {
                Ok(scheme) => Some(scheme),
                Err(hangar_runner::Error::SchemeRequired(_)) => {
                    return Err(NeedsChoice {
                        kind: ChoiceKind::Scheme,
                        options: schemes.iter().map(|s| (s.clone(), s.clone())).collect(),
                    }
                    .into());
                }
                Err(err) => return Err(err.into()),
            }
        }
        _ => None,
    };
    // The serial of a running emulator changes per boot: look it up by AVD when not known.
    if let Some(avd) = &target.avd
        && !target.id.starts_with("emulator-")
        && let Some(found) = crate::adapters::devices::android::discovery::scan()
            .into_iter()
            .find(|d| &d.avd_id == avd)
    {
        target.id = found.serial();
    }
    let plan = hangar_runner::plan(&hangar_runner::PlanInput {
        stack,
        target: &target,
        port,
        scheme: scheme.as_deref(),
        build_dir,
    })?;
    let setup = hangar_runner::setup(stack, root);
    let sdk = crate::adapters::devices::android::AndroidSdk::locate().ok();
    let mut path_dirs: Vec<String> = Vec::new();
    let mut env = Vec::new();
    if let Some(sdk) = &sdk {
        for dir in ["platform-tools", "emulator"] {
            path_dirs.push(sdk.root.join(dir).display().to_string());
        }
        env.push(("ANDROID_HOME".to_owned(), sdk.root.display().to_string()));
    }
    path_dirs.extend(path.or_else(|| std::env::var("PATH").ok()));
    env.push(("PATH".to_owned(), path_dirs.join(":")));
    Ok(Prepared {
        plan,
        setup,
        stack: stack.label(),
        stack_key: stack.key(),
        port,
        scheme,
        target,
        adb: sdk.map(|s| s.adb),
        env,
    })
}

/// Runs `command` through the user's shell (so `npx` and friends resolve as in a terminal).
pub(super) fn spawn_command(
    command: &hangar_runner::Command,
    root: &Path,
    env: &[(String, String)],
    hook: hangar_agents::UpdateHook,
) -> anyhow::Result<AgentHandle> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let line = command.shell_line();
    let adapter = PtyAdapter::new("run", "Run", shell).with_args(["-c", line.as_str()]);
    let mut ctx = SpawnContext::new(root.join(&command.cwd));
    ctx.env.extend(env.iter().cloned());
    ctx.env.extend(command.env.iter().cloned());
    tracing::info!(%line, cwd = %ctx.cwd.display(), "run command");
    adapter
        .launch(&ctx)
        .map_err(anyhow::Error::new)
        .and_then(|launch| Ok(crate::pty::spawn(launch, hook)?))
        .with_context(|| format!("failed to start `{line}`"))
}
