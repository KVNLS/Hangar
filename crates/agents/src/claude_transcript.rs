//! Claude Code's session transcript (`~/.claude/projects/<cwd>/<session>.jsonl`), read to give a
//! terminal Claude Code agent a transcript like a structured one. One JSON object per line:
//! `type` `user` / `assistant` carry `message.content` (a string, or blocks `text`, `tool_use`,
//! `tool_result`, `thinking`); every other type (attachments, system, cost, titles) is ignored, as
//! are subagent lines (`isSidechain`). Lines that do not parse are skipped: the format is Claude
//! Code's own and may change.

use std::collections::HashMap;

use serde_json::Value;

/// One step of the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    User(String),
    Agent(String),
    Tool {
        /// Tool name and its main argument, e.g. `Bash: cargo test`.
        title: String,
        /// `in_progress`, `completed` or `failed`.
        status: &'static str,
    },
}

/// Longest tool argument kept in a title.
const ARG_CHARS: usize = 80;

/// The conversation in `jsonl`, oldest first.
pub fn parse(jsonl: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    // tool_use id → index of its entry, to mark it done when its result arrives.
    let mut tools: HashMap<String, usize> = HashMap::new();
    for line in jsonl.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let kind = value.get("type").and_then(Value::as_str);
        let content = value.get("message").and_then(|m| m.get("content"));
        match (kind, content) {
            (Some("user"), Some(Value::String(text))) => push_text(&mut entries, Entry::User, text),
            (Some("assistant"), Some(Value::String(text))) => {
                push_text(&mut entries, Entry::Agent, text)
            }
            (Some(role @ ("user" | "assistant")), Some(Value::Array(blocks))) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            let make = if role == "user" {
                                Entry::User
                            } else {
                                Entry::Agent
                            };
                            push_text(&mut entries, make, text);
                        }
                        Some("tool_use") => {
                            if let Some(id) = block.get("id").and_then(Value::as_str) {
                                tools.insert(id.to_owned(), entries.len());
                            }
                            entries.push(Entry::Tool {
                                title: tool_title(block),
                                status: "in_progress",
                            });
                        }
                        Some("tool_result") => {
                            let failed =
                                block.get("is_error").and_then(Value::as_bool) == Some(true);
                            let index = block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .and_then(|id| tools.get(id));
                            if let Some(Entry::Tool { status, .. }) =
                                index.and_then(|i| entries.get_mut(*i))
                            {
                                *status = if failed { "failed" } else { "completed" };
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    entries
}

fn push_text(entries: &mut Vec<Entry>, make: fn(String) -> Entry, text: &str) {
    let text = text.trim();
    // Claude Code wraps local command echoes and reminders in tags; they are not conversation.
    if !text.is_empty() && !text.starts_with('<') {
        entries.push(make(text.to_owned()));
    }
}

/// `Name: main argument`, the argument cut to one line.
fn tool_title(block: &Value) -> String {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
    let input = block.get("input");
    let arg = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|key| input.and_then(|i| i.get(*key)).and_then(Value::as_str));
    match arg {
        Some(arg) => {
            let line = arg.lines().next().unwrap_or("").trim();
            let cut: String = line.chars().take(ARG_CHARS).collect();
            let more = if cut.len() < line.len() { "…" } else { "" };
            format!("{name}: {cut}{more}")
        }
        None => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines shaped like a real transcript (trimmed), including types Hangar ignores.
    const SAMPLE: &str = r#"{"type":"ai-title","title":"Note"}
{"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"Write note.txt"}]}}
{"type":"attachment","isSidechain":false}
{"type":"assistant","isSidechain":false,"message":{"role":"assistant","content":[{"type":"thinking","thinking":"…"}]}}
{"type":"assistant","isSidechain":false,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo done > note.txt\nls","description":"Write"}}]}}
{"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"","is_error":false}]}}
{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"subagent chatter"}]}}
{"type":"assistant","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}
{"type":"user","isSidechain":false,"message":{"role":"user","content":"<local-command-stdout>x</local-command-stdout>"}}
{"type":"system","subtype":"turn_duration"}
not json
{"type":"cost-state"}"#;

    #[test]
    fn keeps_the_conversation_in_order() {
        assert_eq!(
            parse(SAMPLE),
            [
                Entry::User("Write note.txt".into()),
                Entry::Tool {
                    title: "Bash: echo done > note.txt".into(),
                    status: "completed",
                },
                Entry::Agent("Done.".into()),
            ]
        );
    }

    #[test]
    fn tools_without_a_result_are_running_and_errors_fail() {
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Read","input":{"file_path":"/x/y.rs"}},{"type":"tool_use","id":"b","name":"WebSearch","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b","is_error":true}]}}
{"type":"assistant","message":{"content":"plain string reply"}}"#;
        assert_eq!(
            parse(jsonl),
            [
                Entry::Tool {
                    title: "Read: /x/y.rs".into(),
                    status: "in_progress",
                },
                Entry::Tool {
                    title: "WebSearch".into(),
                    status: "failed",
                },
                Entry::Agent("plain string reply".into()),
            ]
        );
    }

    #[test]
    fn long_arguments_are_cut() {
        let long = "x".repeat(200);
        let block = serde_json::json!({ "name": "Bash", "input": { "command": long } });
        assert_eq!(tool_title(&block), format!("Bash: {}…", "x".repeat(80)));
    }
}
