//! How much of a long agent message the chat lays out. Text layout of a multi-megabyte message
//! pins the main thread, so only its head is shown; the transcript keeps the whole text.

/// Bytes of one agent message the chat shows.
pub(super) const MAX_SHOWN_BYTES: usize = 64 * 1024;

/// How many leading bytes of `chunk` to show when `shown` bytes of its message already are.
pub(super) fn shown_part(shown: usize, chunk: &str) -> usize {
    chunk.floor_char_boundary(MAX_SHOWN_BYTES.saturating_sub(shown))
}

/// The line under a message whose tail is not shown.
pub(super) fn hidden_notice(hidden: usize) -> String {
    let kb = hidden.div_ceil(1024);
    if kb >= 1024 {
        format!("… {:.1} MB more not shown", kb as f64 / 1024.)
    } else {
        format!("… {kb} KB more not shown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_shown_up_to_the_cap() {
        assert_eq!(shown_part(0, "hello"), 5);
        assert_eq!(shown_part(MAX_SHOWN_BYTES - 2, "hello"), 2);
        assert_eq!(shown_part(MAX_SHOWN_BYTES, "hello"), 0);
        assert_eq!(shown_part(MAX_SHOWN_BYTES + 10, "hello"), 0);
        // Never cuts inside a character.
        assert_eq!(shown_part(MAX_SHOWN_BYTES - 1, "é"), 0);
    }

    #[test]
    fn notice_names_the_hidden_size() {
        assert_eq!(hidden_notice(1), "… 1 KB more not shown");
        assert_eq!(hidden_notice(30 * 1024 * 1024), "… 30.0 MB more not shown");
    }
}
