//! Agent sessions: recording how a start went, how a session ended, and stops made by Hangar.
//! Generic over the running agent so this layer never sees PTYs or ACP connections.

use std::error::Error as StdError;

use hangar_core::changes::DiffStat;
use hangar_core::ports::{PortError, Store};
use hangar_core::{Session, SessionId, SessionStatus, Timestamp};

pub type BoxError = Box<dyn StdError + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to start {agent}")]
    Start {
        agent: String,
        #[source]
        source: BoxError,
    },
    #[error("the failure could not be saved: {record}")]
    FailureNotRecorded {
        record: PortError,
        #[source]
        source: Box<SessionError>,
    },
    #[error("the agent started but its session could not be saved")]
    NotRecorded(#[source] PortError),
    #[error("failed to record the stopped session")]
    Stop(#[source] PortError),
    #[error(transparent)]
    Port(#[from] PortError),
}

/// An agent that started, with its recorded session.
#[derive(Debug)]
pub struct Started<T> {
    pub session: Session,
    pub agent: T,
    /// The agent runs but its session row could not be written.
    pub record_error: Option<SessionError>,
}

#[derive(Clone, Copy)]
pub struct Sessions<'a> {
    store: &'a dyn Store,
}

impl<'a> Sessions<'a> {
    pub fn new(store: &'a dyn Store) -> Self {
        Self { store }
    }

    /// Records the outcome of starting an agent: `Running` with the command that runs, or
    /// `Failed` with the whole error chain. A failed start is recorded too, then returned.
    pub fn record_start<T, E: Into<BoxError>>(
        &self,
        mut session: Session,
        agent_name: &str,
        spawned: Result<(T, String), E>,
    ) -> Result<Started<T>, SessionError> {
        match spawned {
            Ok((agent, command)) => {
                session.command = command;
                session.status = SessionStatus::Running;
                let record_error = self
                    .store
                    .insert_session(&session)
                    .err()
                    .map(SessionError::NotRecorded);
                Ok(Started {
                    session,
                    agent,
                    record_error,
                })
            }
            Err(err) => {
                let err = err.into();
                session.status = SessionStatus::Failed {
                    message: chain(err.as_ref()),
                };
                session.ended_at = Some(Timestamp::now());
                let err = SessionError::Start {
                    agent: agent_name.to_owned(),
                    source: err,
                };
                match self.store.insert_session(&session) {
                    Ok(()) => Err(err),
                    Err(record) => Err(SessionError::FailureNotRecorded {
                        record,
                        source: Box::new(err),
                    }),
                }
            }
        }
    }

    /// Records how the session ended, with the workspace's diff at that moment when known.
    pub fn finish(
        &self,
        id: SessionId,
        status: &SessionStatus,
        changes: Option<DiffStat>,
    ) -> Result<Session, SessionError> {
        Ok(self.store.finish_session(id, status, changes)?)
    }

    /// Records that a running session was stopped by Hangar (archive, delete).
    pub fn stop(
        &self,
        id: SessionId,
        reason: &str,
        changes: Option<DiffStat>,
    ) -> Result<(), SessionError> {
        self.store
            .finish_session(id, &stopped(reason), changes)
            .map_err(SessionError::Stop)?;
        Ok(())
    }

    /// Sessions of every workspace that ended at or after `since`, the latest ended first.
    pub fn ended_since(&self, since: Timestamp) -> Result<Vec<Session>, SessionError> {
        Ok(self.store.list_sessions_since(since)?)
    }
}

const STOPPED_PREFIX: &str = "stopped: ";

/// The status of a session Hangar stopped.
pub fn stopped(reason: &str) -> SessionStatus {
    SessionStatus::Failed {
        message: format!("{STOPPED_PREFIX}{reason}"),
    }
}

/// Whether `status` is one recorded by [`stopped`].
pub fn is_stopped(status: &SessionStatus) -> bool {
    matches!(status, SessionStatus::Failed { message } if message.starts_with(STOPPED_PREFIX))
}

/// `outer: inner: …`, like `anyhow`'s `{:#}`.
fn chain(err: &(dyn StdError + 'static)) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(next) = source {
        text.push_str(": ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use hangar_core::{AgentKind, WorkspaceId};

    use super::*;
    use crate::fakes::FakeStore;

    fn session() -> Session {
        Session::new(WorkspaceId::new(), AgentKind::Shell)
    }

    #[derive(Debug, thiserror::Error)]
    #[error("spawn failed")]
    struct Outer(#[source] std::io::Error);

    #[test]
    fn a_start_records_running_with_the_command() {
        let store = FakeStore::default();
        let started = Sessions::new(&store)
            .record_start(
                session(),
                "Shell",
                Ok::<_, BoxError>(("agent", "/bin/sh".to_owned())),
            )
            .unwrap();
        assert_eq!(started.agent, "agent");
        assert_eq!(started.session.status, SessionStatus::Running);
        assert_eq!(started.session.command, "/bin/sh");
        assert!(started.record_error.is_none());
        assert_eq!(store.sessions.lock().unwrap().clone(), [started.session]);
    }

    #[test]
    fn a_failed_start_is_recorded_with_the_whole_chain() {
        let store = FakeStore::default();
        let cause = Outer(std::io::Error::other("no such file"));
        let err = Sessions::new(&store)
            .record_start::<(), _>(session(), "Claude Code", Err(cause))
            .unwrap_err();
        assert_eq!(err.to_string(), "failed to start Claude Code");
        let recorded = store.sessions.lock().unwrap()[0].clone();
        assert_eq!(
            recorded.status,
            SessionStatus::Failed {
                message: "spawn failed: no such file".to_owned()
            }
        );
        assert!(recorded.ended_at.is_some());
    }

    #[test]
    fn unsaved_rows_are_reported_without_losing_the_agent_or_the_cause() {
        let store = FakeStore {
            refuse_sessions: true,
            ..FakeStore::default()
        };
        let sessions = Sessions::new(&store);
        let started = sessions
            .record_start(session(), "Shell", Ok::<_, BoxError>(((), String::new())))
            .unwrap();
        assert!(matches!(
            started.record_error,
            Some(SessionError::NotRecorded(_))
        ));

        let err = sessions
            .record_start::<(), _>(session(), "Shell", Err(Outer(std::io::Error::other("x"))))
            .unwrap_err();
        assert_eq!(err.to_string(), "the failure could not be saved: disk full");
        let cause = std::error::Error::source(&err).unwrap();
        assert_eq!(cause.to_string(), "failed to start Shell");
    }

    #[test]
    fn stop_marks_the_session_failed_with_the_reason() {
        let store = FakeStore::default();
        let s = session();
        store.insert_session(&s).unwrap();
        Sessions::new(&store)
            .stop(s.id, "workspace archived", None)
            .unwrap();
        let recorded = store.sessions.lock().unwrap()[0].clone();
        assert_eq!(
            recorded.status,
            SessionStatus::Failed {
                message: "stopped: workspace archived".to_owned()
            }
        );
        assert!(is_stopped(&recorded.status));
        assert!(recorded.ended_at.is_some());
    }

    #[test]
    fn the_prompt_is_recorded_on_start_and_the_diff_on_finish() {
        let store = FakeStore::default();
        let sessions = Sessions::new(&store);
        let mut s = session();
        s.prompt = Some("Fix the login crash".to_owned());
        let started = sessions
            .record_start(s, "Shell", Ok::<_, BoxError>(((), "/bin/sh".to_owned())))
            .unwrap();
        assert_eq!(
            store.sessions.lock().unwrap()[0].prompt.as_deref(),
            Some("Fix the login crash")
        );

        let stat = DiffStat {
            files: 1,
            insertions: 3,
            deletions: 0,
        };
        let id = started.session.id;
        let done = sessions
            .finish(id, &SessionStatus::Exited { code: 0 }, Some(stat))
            .unwrap();
        assert_eq!(done.changes, Some(stat));
        let ended = sessions.ended_since(Timestamp(0)).unwrap();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].id, id);
        assert_eq!(ended[0].changes, Some(stat));
    }

    #[test]
    fn a_stop_keeps_the_diff_it_is_given() {
        let store = FakeStore::default();
        let s = session();
        store.insert_session(&s).unwrap();
        let stat = DiffStat {
            files: 4,
            insertions: 9,
            deletions: 9,
        };
        Sessions::new(&store)
            .stop(s.id, "replaced by a new session", Some(stat))
            .unwrap();
        assert_eq!(store.sessions.lock().unwrap()[0].changes, Some(stat));
    }
}
