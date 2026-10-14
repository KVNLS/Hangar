//! Results of the orchestration tools, with which a lead agent creates child workspaces and
//! follows them.

use serde_json::{Value, json};

use crate::tools::ToolOutput;

/// Longest message text returned by `agent_transcript`, in characters.
const MAX_ENTRY_CHARS: usize = 2000;

/// One child workspace as `workspaces_list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildWorkspace {
    pub id: String,
    pub name: String,
    pub task: Option<String>,
    pub branch: String,
    /// Display name of the running (or last started) agent.
    pub agent: Option<String>,
    /// The agent state as Hangar shows it (`Working`, `Waiting for approval`, …).
    pub state: String,
    /// Lines added and removed against the base branch, when Hangar has computed them.
    pub diff: Option<(usize, usize)>,
    /// The agent waits for a free slot ("Max concurrent agents").
    pub queued: bool,
}

/// Whether the agent of a new workspace runs yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    /// Waiting behind `ahead` other workspaces for a free slot.
    Queued {
        ahead: usize,
    },
    /// The agent could not start; Hangar showed the user why.
    Failed,
}

/// What `workspace_create` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorkspace {
    pub id: String,
    pub name: String,
    pub branch: String,
    /// Display name of the agent started there.
    pub agent: String,
    pub start: StartOutcome,
}

/// One message of a structured agent's conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEntry {
    User(String),
    Agent(String),
    Tool {
        title: String,
        /// `pending`, `in_progress`, `completed` or `failed`.
        status: String,
    },
    Permission {
        title: String,
        /// How it was answered; `None` while the agent waits.
        answer: Option<String>,
    },
    Notice(String),
    /// A terminal agent's recent screen and scrollback text, when no structured transcript exists.
    Screen(String),
}

impl ToolOutput {
    /// The `workspaces_list` result (pretty JSON text).
    pub fn workspaces(children: &[ChildWorkspace]) -> Self {
        let list: Vec<Value> = children
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "name": c.name,
                    "task": c.task,
                    "branch": c.branch,
                    "agent": c.agent,
                    "state": c.state,
                    "diff": c.diff.map(|(added, removed)| json!({ "added": added, "removed": removed })),
                    "queued": c.queued,
                })
            })
            .collect();
        pretty(&json!({ "workspaces": list }))
    }

    /// The `workspace_create` result (pretty JSON text).
    pub fn created(created: &CreatedWorkspace) -> Self {
        let (state, ahead) = match created.start {
            StartOutcome::Started => ("started", None),
            StartOutcome::Queued { ahead } => ("queued", Some(ahead)),
            StartOutcome::Failed => ("failed_to_start", None),
        };
        pretty(&json!({
            "id": created.id,
            "name": created.name,
            "branch": created.branch,
            "agent": created.agent,
            "agent_start": state,
            "queued_behind": ahead,
        }))
    }

    /// The `agent_transcript` result: one block per message, oldest first.
    pub fn transcript(workspace: &str, entries: &[TranscriptEntry]) -> Self {
        if entries.is_empty() {
            return Self::text(format!("“{workspace}” has no messages yet."));
        }
        let blocks: Vec<String> = entries
            .iter()
            .map(|entry| match entry {
                TranscriptEntry::User(text) => format!("[user]\n{}", clip(text)),
                TranscriptEntry::Agent(text) => format!("[agent]\n{}", clip(text)),
                TranscriptEntry::Tool { title, status } => format!("[tool · {status}] {title}"),
                TranscriptEntry::Permission { title, answer } => match answer {
                    Some(answer) => format!("[permission · {answer}] {title}"),
                    None => format!("[permission · waiting for the user] {title}"),
                },
                TranscriptEntry::Notice(text) => format!("[hangar] {text}"),
                TranscriptEntry::Screen(text) => {
                    format!("[terminal screen and scrollback, approximate]\n{text}")
                }
            })
            .collect();
        Self::text(blocks.join("\n\n"))
    }
}

fn pretty(value: &Value) -> ToolOutput {
    ToolOutput::text(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
}

fn clip(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MAX_ENTRY_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Content;

    fn text(out: ToolOutput) -> String {
        match out.content.as_slice() {
            [Content::Text(text)] => text.clone(),
            other => panic!("expected one text, got {other:?}"),
        }
    }

    #[test]
    fn workspaces_are_listed_as_json() {
        let out = text(ToolOutput::workspaces(&[ChildWorkspace {
            id: "abc".into(),
            name: "Login".into(),
            task: Some("Fix login".into()),
            branch: "hangar/login".into(),
            agent: Some("Codex".into()),
            state: "Working".into(),
            diff: Some((12, 3)),
            queued: false,
        }]));
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["workspaces"][0]["name"], "Login");
        assert_eq!(
            value["workspaces"][0]["diff"],
            json!({"added": 12, "removed": 3})
        );
        assert_eq!(value["workspaces"][0]["queued"], false);
        let empty: Value = serde_json::from_str(&text(ToolOutput::workspaces(&[]))).unwrap();
        assert_eq!(empty, json!({ "workspaces": [] }));
    }

    #[test]
    fn created_says_whether_the_agent_runs() {
        let created = |start| {
            let out = text(ToolOutput::created(&CreatedWorkspace {
                id: "abc".into(),
                name: "Login".into(),
                branch: "hangar/login".into(),
                agent: "Claude Code".into(),
                start,
            }));
            serde_json::from_str::<Value>(&out).unwrap()
        };
        assert_eq!(created(StartOutcome::Started)["agent_start"], "started");
        let queued = created(StartOutcome::Queued { ahead: 2 });
        assert_eq!(queued["agent_start"], "queued");
        assert_eq!(queued["queued_behind"], 2);
        assert_eq!(
            created(StartOutcome::Failed)["agent_start"],
            "failed_to_start"
        );
    }

    #[test]
    fn transcript_blocks_and_clipping() {
        let long = "x".repeat(MAX_ENTRY_CHARS + 5);
        let out = text(ToolOutput::transcript(
            "Login",
            &[
                TranscriptEntry::User("Fix it".into()),
                TranscriptEntry::Tool {
                    title: "Edit src/login.rs".into(),
                    status: "completed".into(),
                },
                TranscriptEntry::Permission {
                    title: "Run cargo test".into(),
                    answer: None,
                },
                TranscriptEntry::Agent(long),
            ],
        ));
        assert!(out.starts_with("[user]\nFix it\n\n[tool · completed] Edit src/login.rs\n\n"));
        assert!(out.contains("[permission · waiting for the user] Run cargo test"));
        assert!(out.ends_with(&format!("{}…", "x".repeat(MAX_ENTRY_CHARS))));
        assert_eq!(
            text(ToolOutput::transcript("Login", &[])),
            "“Login” has no messages yet."
        );
    }
}
