use std::path::PathBuf;

use hangar_core::changes::{DiffLine, DiffOptions, FileDiff, FileStatus, Hunk, LineKind};

use crate::error::{Error, Result};
use crate::git::{bytes_to_path, bytes_to_string};

/// One entry of `git diff --raw -z`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RawEntry {
    old_mode: u32,
    new_mode: u32,
    status: u8,
    path: PathBuf,
    old_path: Option<PathBuf>,
}

/// Parses `git diff --raw -p -z` output: NUL-separated raw entries, an empty field, then the
/// textual patch. Raw entries give exact statuses and modes; the patch gives hunks. They are
/// joined by path because git may split one raw entry (a type change) into two patch sections.
pub fn parse_patch(out: &[u8], opts: &DiffOptions) -> Result<Vec<FileDiff>> {
    let (raw, patch) = split_raw(out)?;
    let mut files: Vec<FileDiff> = Vec::new();
    for section in parse_sections(patch, opts)? {
        match files.iter_mut().find(|f| f.path == section.path) {
            Some(existing) => merge_section(existing, section),
            None => files.push(section),
        }
    }
    for entry in raw {
        let index = match files.iter().position(|f| f.path == entry.path) {
            Some(index) => index,
            None => {
                files.push(FileDiff::new(entry.path.clone(), FileStatus::Modified));
                files.len() - 1
            }
        };
        let Some(file) = files.get_mut(index) else {
            continue;
        };
        // A conflict yields a `U` entry plus a regular one for the worktree content.
        if file.status == FileStatus::Unmerged {
            continue;
        }
        file.status = match entry.status {
            b'A' => FileStatus::Added,
            b'D' => FileStatus::Deleted,
            b'R' => FileStatus::Renamed,
            b'C' => FileStatus::Copied,
            b'T' => FileStatus::TypeChanged,
            b'U' => FileStatus::Unmerged,
            _ => FileStatus::Modified,
        };
        file.old_mode = (entry.old_mode != 0).then_some(entry.old_mode);
        file.new_mode = (entry.new_mode != 0).then_some(entry.new_mode);
        if entry.old_path.is_some() {
            file.old_path = entry.old_path;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// A type change arrives as a deletion section followed by an addition section.
fn merge_section(existing: &mut FileDiff, section: FileDiff) {
    existing.status = FileStatus::TypeChanged;
    existing.new_mode = section.new_mode.or(existing.new_mode);
    existing.binary |= section.binary;
    existing.truncated |= section.truncated;
    existing.insertions += section.insertions;
    existing.deletions += section.deletions;
    existing.hunks.extend(section.hunks);
}

fn split_raw(out: &[u8]) -> Result<(Vec<RawEntry>, &[u8])> {
    let mut entries = Vec::new();
    let mut rest = out;
    while rest.first() == Some(&b':') {
        let (meta, after) = take_field(rest)?;
        let bad = || Error::Parse(format!("bad raw diff entry: {}", bytes_to_string(meta)));
        let mut parts = meta[1..].split(|&b| b == b' ');
        let (Some(old_mode), Some(new_mode), Some(_), Some(_), Some(status)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(bad());
        };
        let status = *status.first().ok_or_else(bad)?;
        let mode = |raw: &[u8]| u32::from_str_radix(&bytes_to_string(raw), 8).map_err(|_| bad());
        let (first, after) = take_field(after)?;
        let (path, old_path, after) = if matches!(status, b'R' | b'C') {
            let (second, after) = take_field(after)?;
            (bytes_to_path(second), Some(bytes_to_path(first)), after)
        } else {
            (bytes_to_path(first), None, after)
        };
        entries.push(RawEntry {
            old_mode: mode(old_mode)?,
            new_mode: mode(new_mode)?,
            status,
            path,
            old_path,
        });
        rest = after;
    }
    if !entries.is_empty() {
        rest = rest.strip_prefix(b"\0").unwrap_or(rest);
    }
    Ok((entries, rest))
}

fn take_field(input: &[u8]) -> Result<(&[u8], &[u8])> {
    let end = input
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| Error::Parse("unterminated raw diff field".into()))?;
    Ok((&input[..end], &input[end + 1..]))
}

fn parse_sections(patch: &[u8], opts: &DiffOptions) -> Result<Vec<FileDiff>> {
    let mut lines = patch
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .peekable();
    let mut files = Vec::new();
    while let Some(line) = lines.next() {
        let Some(header) = line.strip_prefix(b"diff --git ") else {
            if line.is_empty() || line.starts_with(b"* Unmerged path ") {
                continue;
            }
            return Err(Error::Parse(format!(
                "unexpected patch line: {}",
                bytes_to_string(line)
            )));
        };
        let (a, b) = split_header_paths(header)?;
        let mut file = FileDiff::new(b, FileStatus::Modified);
        let mut rename_from = None;
        let mut rename_to = None;
        while let Some(&next) = lines.peek() {
            if next.starts_with(b"diff --git ") || next.starts_with(b"@@ ") {
                break;
            }
            lines.next();
            if let Some(mode) = next.strip_prefix(b"new file mode ") {
                file.status = FileStatus::Added;
                file.new_mode = parse_mode(mode);
            } else if let Some(mode) = next.strip_prefix(b"deleted file mode ") {
                file.status = FileStatus::Deleted;
                file.old_mode = parse_mode(mode);
            } else if let Some(mode) = next.strip_prefix(b"old mode ") {
                file.old_mode = parse_mode(mode);
            } else if let Some(mode) = next.strip_prefix(b"new mode ") {
                file.new_mode = parse_mode(mode);
            } else if let Some(p) = next.strip_prefix(b"rename from ") {
                file.status = FileStatus::Renamed;
                rename_from = Some(unquote(p)?);
            } else if let Some(p) = next.strip_prefix(b"rename to ") {
                rename_to = Some(unquote(p)?);
            } else if let Some(p) = next.strip_prefix(b"copy from ") {
                file.status = FileStatus::Copied;
                rename_from = Some(unquote(p)?);
            } else if let Some(p) = next.strip_prefix(b"copy to ") {
                rename_to = Some(unquote(p)?);
            } else if next.starts_with(b"Binary files ") || next == b"GIT binary patch" {
                file.binary = true;
            }
        }
        if let Some(to) = rename_to {
            file.path = to;
        }
        if file.status == FileStatus::Deleted {
            file.path = a.clone();
        }
        file.old_path = rename_from;
        parse_hunks(&mut lines, &mut file, opts)?;
        files.push(file);
    }
    Ok(files)
}

fn parse_hunks<'a, I>(
    lines: &mut std::iter::Peekable<I>,
    file: &mut FileDiff,
    opts: &DiffOptions,
) -> Result<()>
where
    I: Iterator<Item = &'a [u8]>,
{
    let mut kept = 0usize;
    while let Some(header) = lines.next_if(|l| l.starts_with(b"@@ ")) {
        let mut hunk = parse_hunk_header(header)?;
        let (mut old_left, mut new_left) = (hunk.old_lines, hunk.new_lines);
        let (mut old_no, mut new_no) = (hunk.old_start, hunk.new_start);
        while let Some(&line) = lines.peek() {
            let (kind, text) = match line.split_first() {
                Some((b'\\', _)) => (LineKind::NoNewlineAtEof, &line[1..]),
                _ if old_left == 0 && new_left == 0 => break,
                Some((b' ', text)) => (LineKind::Context, text),
                Some((b'-', text)) => (LineKind::Removed, text),
                Some((b'+', text)) => (LineKind::Added, text),
                // `diff.suppressBlankEmpty` prints empty context lines without the space.
                None => (LineKind::Context, line),
                Some(_) => {
                    return Err(Error::Parse(format!(
                        "unexpected line in hunk of {}: {}",
                        file.path.display(),
                        bytes_to_string(line)
                    )));
                }
            };
            lines.next();
            let (old_lineno, new_lineno) = match kind {
                LineKind::Context => (Some(old_no), Some(new_no)),
                LineKind::Removed => (Some(old_no), None),
                LineKind::Added => (None, Some(new_no)),
                LineKind::NoNewlineAtEof => (None, None),
            };
            let consumes_old = matches!(kind, LineKind::Context | LineKind::Removed);
            let consumes_new = matches!(kind, LineKind::Context | LineKind::Added);
            if (consumes_old && old_left == 0) || (consumes_new && new_left == 0) {
                return Err(Error::Parse(format!(
                    "hunk of {} longer than its header",
                    file.path.display()
                )));
            }
            if consumes_old {
                old_left -= 1;
                old_no += 1;
            }
            if consumes_new {
                new_left -= 1;
                new_no += 1;
            }
            match kind {
                LineKind::Added => file.insertions += 1,
                LineKind::Removed => file.deletions += 1,
                _ => {}
            }
            if opts.max_lines_per_file.is_some_and(|max| kept >= max) {
                file.truncated = true;
                continue;
            }
            kept += 1;
            let text = match kind {
                LineKind::NoNewlineAtEof => bytes_to_string(text.trim_ascii_start()),
                _ => bytes_to_string(text),
            };
            hunk.lines.push(DiffLine {
                kind,
                old_lineno,
                new_lineno,
                text,
            });
        }
        if old_left != 0 || new_left != 0 {
            return Err(Error::Parse(format!(
                "truncated hunk in {}",
                file.path.display()
            )));
        }
        if !hunk.lines.is_empty() {
            file.hunks.push(hunk);
        }
    }
    Ok(())
}

/// Parses `@@ -a[,b] +c[,d] @@[ heading]`.
fn parse_hunk_header(line: &[u8]) -> Result<Hunk> {
    let text = bytes_to_string(line);
    let bad = || Error::Parse(format!("bad hunk header: {text}"));
    let rest = text.strip_prefix("@@ -").ok_or_else(bad)?;
    let (ranges, heading) = rest.split_once(" @@").ok_or_else(bad)?;
    let (old, new) = ranges.split_once(" +").ok_or_else(bad)?;
    let range = |r: &str| -> Result<(u32, u32)> {
        let (start, len) = r.split_once(',').unwrap_or((r, "1"));
        Ok((
            start.parse().map_err(|_| bad())?,
            len.parse().map_err(|_| bad())?,
        ))
    };
    let (old_start, old_lines) = range(old)?;
    let (new_start, new_lines) = range(new)?;
    Ok(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        header: heading.strip_prefix(' ').unwrap_or(heading).to_owned(),
        lines: Vec::new(),
    })
}

fn parse_mode(raw: &[u8]) -> Option<u32> {
    u32::from_str_radix(bytes_to_string(raw).trim(), 8).ok()
}

/// Splits `a/<old> b/<new>` from a `diff --git` line. Unquoted names are only ambiguous when
/// they contain ` b/`; git always prints both names the same for non-renames, so split in half.
fn split_header_paths(header: &[u8]) -> Result<(PathBuf, PathBuf)> {
    let bad = || Error::Parse(format!("bad diff header: {}", bytes_to_string(header)));
    if header.first() == Some(&b'"') {
        let (a, rest) = take_quoted(header)?;
        let rest = rest.strip_prefix(b" ").ok_or_else(bad)?;
        let b = if rest.first() == Some(&b'"') {
            take_quoted(rest)?.0
        } else {
            rest.to_vec()
        };
        return Ok((strip_prefix(&a, b"a/")?, strip_prefix(&b, b"b/")?));
    }
    if let Some(pos) = header.windows(2).position(|w| w == b" \"") {
        let (b, _) = take_quoted(&header[pos + 1..])?;
        return Ok((
            strip_prefix(&header[..pos], b"a/")?,
            strip_prefix(&b, b"b/")?,
        ));
    }
    if header.len() >= 5 && (header.len() - 5).is_multiple_of(2) {
        let half = (header.len() - 5) / 2;
        let (a, b) = header.split_at(half + 2);
        if b.strip_prefix(b" b/") == Some(&a[2..]) && a.starts_with(b"a/") {
            let path = bytes_to_path(&a[2..]);
            return Ok((path.clone(), path));
        }
    }
    let pos = header
        .windows(3)
        .position(|w| w == b" b/")
        .ok_or_else(bad)?;
    Ok((
        strip_prefix(&header[..pos], b"a/")?,
        bytes_to_path(&header[pos + 3..]),
    ))
}

fn strip_prefix(raw: &[u8], prefix: &[u8]) -> Result<PathBuf> {
    raw.strip_prefix(prefix)
        .map(bytes_to_path)
        .ok_or_else(|| Error::Parse(format!("missing {prefix:?} in {}", bytes_to_string(raw))))
}

fn unquote(raw: &[u8]) -> Result<PathBuf> {
    if raw.first() == Some(&b'"') {
        Ok(bytes_to_path(&take_quoted(raw)?.0))
    } else {
        Ok(bytes_to_path(raw))
    }
}

/// Decodes a C-style quoted name as git writes it (`"a\tb\303\251"`) and returns the rest.
fn take_quoted(raw: &[u8]) -> Result<(Vec<u8>, &[u8])> {
    let bad = || Error::Parse(format!("bad quoted path: {}", bytes_to_string(raw)));
    let mut out = Vec::new();
    let mut i = 1;
    while let Some(&c) = raw.get(i) {
        i += 1;
        match c {
            b'"' => return Ok((out, &raw[i..])),
            b'\\' => {
                let esc = *raw.get(i).ok_or_else(bad)?;
                i += 1;
                out.push(match esc {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    b'0'..=b'7' => {
                        let digits = raw.get(i - 1..i + 2).ok_or_else(bad)?;
                        i += 2;
                        let mut value = 0u32;
                        for &d in digits {
                            if !(b'0'..=b'7').contains(&d) {
                                return Err(bad());
                            }
                            value = value * 8 + u32::from(d - b'0');
                        }
                        u8::try_from(value).map_err(|_| bad())?
                    }
                    other => other,
                });
            }
            other => out.push(other),
        }
    }
    Err(bad())
}

#[cfg(test)]
mod tests;
