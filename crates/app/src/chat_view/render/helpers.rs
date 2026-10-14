use gpui_kit::assets::IconName as Lucide;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::acp::protocol::ToolContent;

use super::MAX_CARD_LINES;
use crate::changes::model::DisplayLine;
use crate::ui::{ActiveTokens as _, TypeScale as _};

/// Icon and short verb for an ACP tool kind.
pub(super) fn tool_kind(kind: &str) -> (Lucide, &'static str) {
    match kind {
        "read" => (Lucide::FileText, "Read"),
        "edit" => (Lucide::Pencil, "Edit"),
        "delete" => (Lucide::Trash, "Delete"),
        "move" => (Lucide::Move, "Move"),
        "search" => (Lucide::Search, "Search"),
        "execute" => (Lucide::SquareTerminal, "Run"),
        "think" => (Lucide::Brain, "Think"),
        "fetch" => (Lucide::Globe, "Fetch"),
        "switch_mode" => (Lucide::ArrowRightLeft, "Mode"),
        _ => (Lucide::Wrench, "Tool"),
    }
}

/// `title` without a leading `verb` the card header already shows ("Read main.rs" → "main.rs").
pub(super) fn without_verb<'a>(title: &'a str, verb: &str) -> &'a str {
    match title.get(..verb.len()) {
        Some(head) if head.eq_ignore_ascii_case(verb) && title[verb.len()..].starts_with(' ') => {
            title[verb.len()..].trim_start()
        }
        _ => title,
    }
}

/// `+a −r` over the diffs of a tool call, if it has any.
pub(super) fn diff_stat(content: &[ToolContent]) -> Option<String> {
    let mut added = 0;
    let mut removed = 0;
    let mut any = false;
    for item in content {
        if let ToolContent::Diff {
            old_text, new_text, ..
        } = item
        {
            any = true;
            for line in diff_lines(old_text.as_deref().unwrap_or_default(), new_text) {
                match line.0 {
                    '+' => added += 1,
                    '-' => removed += 1,
                    _ => {}
                }
            }
        }
    }
    any.then(|| format!("+{added} −{removed}"))
}

/// The changed middle of `old` → `new` (common leading and trailing lines dropped), as removed
/// lines then added lines.
pub(super) fn diff_lines<'a>(old: &'a str, new: &'a str) -> Vec<(char, &'a str)> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    old[prefix..old.len() - suffix]
        .iter()
        .map(|l| ('-', *l))
        .chain(new[prefix..new.len() - suffix].iter().map(|l| ('+', *l)))
        .collect()
}

/// The content area of a tool card: output text, diffs, or nothing.
pub(super) fn tool_body(
    content: &[ToolContent],
    root: &std::path::Path,
    cx: &App,
) -> Option<AnyElement> {
    let t = cx.tokens();
    let mut lines: Vec<AnyElement> = Vec::new();
    let mut hidden = 0usize;
    let mut push = |line: AnyElement, lines: &mut Vec<AnyElement>| {
        if lines.len() < MAX_CARD_LINES {
            lines.push(line);
        } else {
            hidden += 1;
        }
    };
    for item in content {
        match item {
            ToolContent::Text(text) => {
                // Agents wrap command output in Markdown fences; the card is already code.
                for line in text.lines().filter(|l| !l.trim_start().starts_with("```")) {
                    push(
                        div()
                            .px(px(10.))
                            .whitespace_nowrap()
                            .text_color(t.tx2)
                            .child(DisplayLine::new(line).text)
                            .into_any_element(),
                        &mut lines,
                    );
                }
            }
            ToolContent::Diff {
                path,
                old_text,
                new_text,
            } => {
                push(
                    div()
                        .px(px(10.))
                        .text_color(t.tx3)
                        .child(
                            std::path::Path::new(path)
                                .strip_prefix(root)
                                .unwrap_or(std::path::Path::new(path))
                                .display()
                                .to_string(),
                        )
                        .into_any_element(),
                    &mut lines,
                );
                for (sign, line) in diff_lines(old_text.as_deref().unwrap_or_default(), new_text) {
                    let (bg, fg) = if sign == '+' {
                        (t.addbg, t.grn)
                    } else {
                        (t.delbg, t.red)
                    };
                    push(
                        div()
                            .flex()
                            .px(px(10.))
                            .bg(bg)
                            .whitespace_nowrap()
                            .child(div().w(px(14.)).text_color(fg).child(sign.to_string()))
                            .child(line.to_owned())
                            .into_any_element(),
                        &mut lines,
                    );
                }
            }
            ToolContent::Terminal(id) => push(
                div()
                    .px(px(10.))
                    .text_color(t.tx3)
                    .child(format!("[terminal {id}: output not shown]"))
                    .into_any_element(),
                &mut lines,
            ),
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(
        div()
            .border_t_1()
            .border_color(t.line)
            .py(px(4.))
            .font_mono()
            .text_size(px(11.5))
            .line_height(px(18.))
            .overflow_hidden()
            .children(lines)
            .when(hidden > 0, |d| {
                d.child(
                    div()
                        .px(px(10.))
                        .text_color(t.tx3)
                        .child(format!("… {hidden} more lines")),
                )
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::{diff_lines, without_verb};
    #[test]
    fn verb_is_not_repeated() {
        assert_eq!(without_verb("Read main.rs", "Read"), "main.rs");
        assert_eq!(without_verb("edit a.rs", "Edit"), "a.rs");
        assert_eq!(without_verb("Readme", "Read"), "Readme");
        assert_eq!(without_verb("git status", "Run"), "git status");
    }

    #[test]
    fn diff_drops_common_lines() {
        assert_eq!(
            diff_lines("a\nb\nc\n", "a\nB\nc\n"),
            vec![('-', "b"), ('+', "B")]
        );
        assert_eq!(diff_lines("", "x\ny"), vec![('+', "x"), ('+', "y")]);
        assert!(diff_lines("same", "same").is_empty());
    }
}
