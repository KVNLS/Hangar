//! Headless throughput: `seq 1 1000000` through PtySession + Terminal.
//! For numbers: `cargo test -p hangar-terminal --release --test throughput -- --nocapture`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use hangar_terminal::{GridSize, SpawnOptions, TerminalConfig, TerminalSession};

#[test]
fn seq_one_million_through_pty_and_emulator() {
    run_through_session("seq", SpawnOptions::command("seq", ["1", "1000000"]));
}

/// Same data, but block-buffered: `seq` alone writes one line per syscall to a TTY.
#[test]
fn seq_piped_through_cat_through_pty_and_emulator() {
    run_through_session(
        "seq | cat",
        SpawnOptions::command("/bin/sh", ["-c", "seq 1 1000000 | cat"]),
    );
}

fn run_through_session(label: &str, mut options: SpawnOptions) {
    options.size = GridSize::new(120, 40).unwrap();

    let start = Instant::now();
    let session = TerminalSession::spawn(&options, TerminalConfig::default(), || {}).unwrap();
    let exit = session
        .wait_for_exit(Duration::from_secs(120))
        .expect("seq did not finish in time")
        .unwrap();
    let elapsed = start.elapsed();
    assert!(exit.success(), "{exit:?}");

    let bytes = session.stats().bytes_read();
    // `seq 1 1000000` writes 6_888_900 bytes; the PTY line discipline turns each \n into \r\n.
    assert!(bytes >= 6_888_890 + 1_000_000, "{bytes}");

    let snapshot = session.terminal().snapshot();
    let last = (0..snapshot.rows)
        .rev()
        .map(|r| snapshot.row_text(r))
        .find(|t| !t.is_empty())
        .unwrap();
    // BSD seq (macOS) prints the last value as 1e+06, GNU seq as 1000000.
    assert!(last == "1000000" || last == "1e+06", "{last}");
    assert_eq!(snapshot.history_size, 10_000);

    let mb = bytes as f64 / 1_000_000.0;
    eprintln!(
        "throughput ({label}): {bytes} bytes in {:.3}s = {:.1} MB/s ({} update batches)",
        elapsed.as_secs_f64(),
        mb / elapsed.as_secs_f64(),
        session.stats().batches(),
    );
}

/// Baseline: the same PTY stream drained without parsing, to separate PTY cost from emulation.
#[test]
fn seq_one_million_raw_pty_drain() {
    use hangar_terminal::{PtyEvent, PtySession};

    let start = Instant::now();
    let pty = PtySession::spawn(&SpawnOptions::command("seq", ["1", "1000000"])).unwrap();
    let output = pty.take_output().unwrap();
    let mut bytes = 0usize;
    let mut reads = 0usize;
    while let Ok(PtyEvent::Output(chunk)) = output.recv_timeout(Duration::from_secs(60)) {
        bytes += chunk.len();
        reads += 1;
    }
    let elapsed = start.elapsed();
    assert!(bytes >= 6_888_890 + 1_000_000, "{bytes}");
    eprintln!(
        "raw pty drain: {bytes} bytes in {:.3}s = {:.1} MB/s ({reads} reads, {:.0} B/read)",
        elapsed.as_secs_f64(),
        bytes as f64 / 1_000_000.0 / elapsed.as_secs_f64(),
        bytes as f64 / reads as f64,
    );
}

/// Emulator alone: equivalent bytes fed from memory in 64 KiB chunks.
#[test]
fn seq_one_million_parse_only() {
    use hangar_terminal::Terminal;

    let mut data = Vec::with_capacity(8_000_000);
    for i in 1..=1_000_000u32 {
        data.extend_from_slice(i.to_string().as_bytes());
        data.extend_from_slice(b"\r\n");
    }
    let mut terminal = Terminal::new(GridSize::new(120, 40).unwrap(), TerminalConfig::default());
    let start = Instant::now();
    for chunk in data.chunks(64 * 1024) {
        terminal.advance(chunk);
    }
    let elapsed = start.elapsed();
    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.row_text(snapshot.rows - 2), "1000000");
    eprintln!(
        "parse only: {} bytes in {:.3}s = {:.1} MB/s",
        data.len(),
        elapsed.as_secs_f64(),
        data.len() as f64 / 1_000_000.0 / elapsed.as_secs_f64(),
    );
}
