//! UI wording for device sharing: ordinals, durations and the queue lines.

use std::time::Duration;

use hangar_core::WorkspaceId;

use super::Member;

/// "1st", "2nd", "11th" for a 1-based place.
pub fn ordinal(n: usize) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// "~30 s" or "~2 min", rounding minutes up.
pub fn approx(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("~{s} s")
    } else {
        format!("~{} min", s.div_ceil(60))
    }
}

/// "waiting for device (2nd)" for a 0-based queue position.
pub fn waiting_label(position: Option<usize>) -> String {
    match position {
        Some(p) => format!("waiting for device ({})", ordinal(p + 1)),
        None => "waiting for device".to_owned(),
    }
}

/// How long the holder keeps the device.
pub fn frees_line(left: Option<Duration>) -> String {
    match left {
        Some(d) => format!("frees in {}", approx(d)),
        None => "until it is released".to_owned(),
    }
}

/// "in use by ws-b · 2 waiting" for Settings › Devices.
pub fn holder_line(holder: &str, queue_len: usize) -> String {
    match queue_len {
        0 => format!("in use by {holder}"),
        n => format!("in use by {holder} · {n} waiting"),
    }
}

/// "1. ws-a, 2. ws-b (you)" in turn order.
pub fn queue_line(queue: &[Member], you: WorkspaceId) -> String {
    queue
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let you = if m.id == you { " (you)" } else { "" };
            format!("{}. {}{you}", i + 1, m.name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinals_handle_the_teens() {
        let got: Vec<_> = [1, 2, 3, 4, 11, 12, 13, 21, 22].map(ordinal).into();
        assert_eq!(
            got,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd"
            ]
        );
    }

    #[test]
    fn durations_switch_to_minutes_rounding_up() {
        assert_eq!(approx(Duration::from_secs(30)), "~30 s");
        assert_eq!(approx(Duration::from_secs(60)), "~1 min");
        assert_eq!(approx(Duration::from_secs(61)), "~2 min");
    }

    #[test]
    fn waiting_and_holder_lines() {
        assert_eq!(waiting_label(Some(1)), "waiting for device (2nd)");
        assert_eq!(waiting_label(None), "waiting for device");
        assert_eq!(holder_line("ws-b", 0), "in use by ws-b");
        assert_eq!(holder_line("ws-b", 2), "in use by ws-b · 2 waiting");
        assert_eq!(frees_line(None), "until it is released");
        assert_eq!(frees_line(Some(Duration::from_secs(70))), "frees in ~2 min");
    }

    #[test]
    fn the_queue_line_marks_you() {
        let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
        let queue = [
            Member {
                id: a,
                name: "a".into(),
            },
            Member {
                id: b,
                name: "b".into(),
            },
        ];
        assert_eq!(queue_line(&queue, b), "1. a, 2. b (you)");
    }
}
