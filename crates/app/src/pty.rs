//! Starts an agent's [`Launch`] in a pseudo-terminal the terminal view can attach to.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use hangar_agents::{Launch, UpdateHook};
use hangar_terminal::{SpawnOptions, TerminalConfig, TerminalError, TerminalSession};

/// A running agent. The app attaches a terminal view to [`AgentHandle::session`].
pub struct AgentHandle {
    agent_id: String,
    program: std::path::PathBuf,
    session: Arc<TerminalSession>,
}

impl AgentHandle {
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Resolved executable that was started.
    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn session(&self) -> &Arc<TerminalSession> {
        &self.session
    }

    pub fn into_session(self) -> Arc<TerminalSession> {
        self.session
    }
}

impl fmt::Debug for AgentHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentHandle")
            .field("agent_id", &self.agent_id)
            .field("program", &self.program)
            .field("pid", &self.session.pid())
            .finish()
    }
}

/// Spawns `launch` at the default grid size (the view resizes it); `on_update` runs on the
/// terminal pump thread after each batch of output.
pub fn spawn(launch: Launch, on_update: UpdateHook) -> Result<AgentHandle, TerminalError> {
    let options = SpawnOptions {
        program: Some(launch.program.clone().into_os_string()),
        args: launch.args.into_iter().map(Into::into).collect(),
        cwd: Some(launch.cwd),
        env: launch
            .env
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
        ..SpawnOptions::default()
    };
    let session = TerminalSession::spawn(&options, TerminalConfig::default(), move || on_update())?;
    Ok(AgentHandle {
        agent_id: launch.agent_id,
        program: launch.program,
        session: Arc::new(session),
    })
}
