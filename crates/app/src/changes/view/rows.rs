//! One row of the diff: unified or split, with gutters, highlighting and counts.

use std::path::Path;

use gpui_kit::component::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::changes::{FileDiff, FileStatus, LineKind};

use crate::changes::model::{DiffRow, DisplayLine, SplitRow, hunk_header};
use crate::ui::components::Delta;
use crate::ui::{ActiveTokens as _, TypeScale as _};

use super::{DIFF_ROW_HEIGHT, GUTTER_WIDTH, Shown};

pub(super) fn render_row(shown: &Shown, ix: usize, cx: &App) -> Option<AnyElement> {
    let t = cx.tokens();
    let row = h_flex()
        .h(px(DIFF_ROW_HEIGHT))
        .min_w_full()
        .flex_none()
        .items_center()
        .whitespace_nowrap();
    let gutter = |n: Option<u32>| {
        div()
            .w(px(GUTTER_WIDTH))
            .flex_shrink_0()
            .pr_1()
            .text_right()
            .text_color(t.tx3)
            .children(n.map(|n| n.to_string()))
    };
    let element = match *shown.rows.get(ix)? {
        DiffRow::Hunk { hunk } => {
            let hunk = shown.file.hunks.get(hunk)?;
            row.bg(t.accbg)
                .text_color(t.acc)
                .child(div().w(px(GUTTER_WIDTH * 2. + 14.)).flex_shrink_0())
                .child(hunk_header(hunk))
                .into_any_element()
        }
        DiffRow::Notice(notice) => row
            .px_2()
            .text_color(t.tx3)
            .child(notice.text())
            .into_any_element(),
        DiffRow::Line { hunk, line } => {
            let line = shown.file.hunks.get(hunk)?.lines.get(line)?;
            let (bg, sign, sign_color) = match line.kind {
                LineKind::Added => (Some(t.addbg), "+", t.grn),
                LineKind::Removed => (Some(t.delbg), "−", t.red),
                LineKind::Context | LineKind::NoNewlineAtEof => (None, " ", t.tx3),
            };
            let body = line_body(shown, ix, line, cx);
            row.when_some(bg, |this, bg| this.bg(bg))
                .child(gutter(line.old_lineno))
                .child(gutter(line.new_lineno))
                .child(
                    div()
                        .w(px(14.))
                        .flex_shrink_0()
                        .text_color(sign_color)
                        .child(sign),
                )
                .child(body)
                .into_any_element()
        }
    };
    Some(element)
}

/// A diff line's text, syntax-highlighted when highlights for unified row `ix` are ready.
pub(super) fn line_body(
    shown: &Shown,
    ix: usize,
    line: &hangar_core::changes::DiffLine,
    cx: &App,
) -> AnyElement {
    if line.kind == LineKind::NoNewlineAtEof {
        return div()
            .text_color(cx.tokens().tx3)
            .child("\\ No newline at end of file")
            .into_any_element();
    }
    let text = DisplayLine::new(&line.text).text;
    let styles = shown
        .styles
        .as_ref()
        .and_then(|s| s.get(ix).cloned().flatten());
    match styles {
        Some(styles) if !styles.is_empty() => StyledText::new(text)
            .with_highlights(styles)
            .into_any_element(),
        _ => SharedString::from(text).into_any_element(),
    }
}

/// One row of the side-by-side diff.
pub(super) fn render_split_row(shown: &Shown, ix: usize, cx: &App) -> Option<AnyElement> {
    let (left, right) = match *shown.split.get(ix)? {
        SplitRow::Full(row) => return render_row(shown, row, cx),
        SplitRow::Pair { left, right } => (left, right),
    };
    let t = cx.tokens();
    let half = |row: Option<usize>, old: bool| {
        let cell = h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .whitespace_nowrap();
        let line = row.and_then(|row| match *shown.rows.get(row)? {
            DiffRow::Line { hunk, line } => shown
                .file
                .hunks
                .get(hunk)?
                .lines
                .get(line)
                .map(|l| (row, l)),
            _ => None,
        });
        let Some((row, line)) = line else {
            return cell.bg(t.hover).into_any_element();
        };
        let (bg, sign, sign_color) = match line.kind {
            LineKind::Added => (Some(t.addbg), "+", t.grn),
            LineKind::Removed => (Some(t.delbg), "−", t.red),
            LineKind::Context | LineKind::NoNewlineAtEof => (None, " ", t.tx3),
        };
        let number = if old {
            line.old_lineno
        } else {
            line.new_lineno
        };
        cell.when_some(bg, |this, bg| this.bg(bg))
            .child(
                div()
                    .w(px(48.))
                    .flex_shrink_0()
                    .pr(px(10.))
                    .text_right()
                    .text_color(t.tx3)
                    .children(number.map(|n| n.to_string())),
            )
            .child(
                div()
                    .w(px(16.))
                    .flex_shrink_0()
                    .text_color(sign_color)
                    .child(sign),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .child(line_body(shown, row, line, cx)),
            )
            .into_any_element()
    };
    Some(
        h_flex()
            .h(px(20.))
            .w_full()
            .child(half(left, true))
            .child(div().w(px(1.)).h_full().bg(t.line))
            .child(half(right, false))
            .into_any_element(),
    )
}

pub(super) fn counts(file: &FileDiff, cx: &App) -> AnyElement {
    if file.binary {
        return div()
            .flex_none()
            .font_mono()
            .text_size(px(11.))
            .text_color(cx.tokens().tx3)
            .child("bin")
            .into_any_element();
    }
    Delta::new(file.insertions as usize, file.deletions as usize).into_any_element()
}

pub(super) fn status_color(status: FileStatus, cx: &App) -> Hsla {
    let t = cx.tokens();
    match status {
        FileStatus::Added | FileStatus::Untracked | FileStatus::Copied => t.grn,
        FileStatus::Deleted | FileStatus::Unmerged => t.red,
        FileStatus::Modified | FileStatus::Renamed | FileStatus::TypeChanged => t.amb,
    }
}

/// `("dir/sub", "name.rs")` for display.
pub(super) fn split_path(path: &Path) -> (String, String) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    (dir, name)
}
