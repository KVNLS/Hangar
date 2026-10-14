//! `HANGAR_TERM_STATS=1` diagnostics: frames painted, PTY throughput and keystroke latency.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hangar_terminal::SessionStats;

pub(super) struct FrameStats {
    session: Arc<SessionStats>,
    frames: u64,
    wakes: u64,
    last_report: Instant,
    last_bytes: u64,
    /// First unanswered keystroke and the PTY byte count at that moment.
    pending_key: Option<(Instant, u64)>,
    latencies: Vec<Duration>,
}

impl FrameStats {
    pub(super) fn new(session: Arc<SessionStats>) -> Self {
        Self {
            last_bytes: session.bytes_read(),
            session,
            frames: 0,
            wakes: 0,
            last_report: Instant::now(),
            pending_key: None,
            latencies: Vec::new(),
        }
    }

    /// Starts a latency measurement unless one is already waiting for output.
    pub(super) fn key_pressed(&mut self) {
        if self.pending_key.is_none() {
            self.pending_key = Some((Instant::now(), self.session.bytes_read()));
        }
    }

    /// Records that PTY output woke the view (each wake requests one redraw).
    pub(super) fn woke(&mut self) {
        self.wakes += 1;
    }

    /// Records a painted frame. `bytes_in_frame` is the PTY byte count read under the same
    /// lock as the snapshot, so it tells exactly which output the frame contains.
    pub(super) fn frame_painted(&mut self, bytes_in_frame: u64) {
        self.frames += 1;
        if let Some((at, bytes_at_key)) = self.pending_key
            && bytes_in_frame > bytes_at_key
        {
            self.latencies.push(at.elapsed());
            self.pending_key = None;
        }
    }

    pub(super) fn report(&mut self, window_visible: bool) {
        let now = Instant::now();
        let secs = now.duration_since(self.last_report).as_secs_f64().max(1e-6);
        let bytes = self.session.bytes_read();
        let fps = self.frames as f64 / secs;
        let wakes = self.wakes as f64 / secs;
        let bps = (bytes - self.last_bytes) as f64 / secs;
        let latency = if self.latencies.is_empty() {
            "n/a".to_owned()
        } else {
            self.latencies.sort();
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            let median = self.latencies[self.latencies.len() / 2];
            let max = self.latencies[self.latencies.len() - 1];
            format!(
                "n={} min={:.1}ms median={:.1}ms max={:.1}ms",
                self.latencies.len(),
                ms(self.latencies[0]),
                ms(median),
                ms(max)
            )
        };
        eprintln!(
            "[hangar-term-stats] frames/s={fps:.1} wakes/s={wakes:.1} pty_bytes/s={bps:.0} key->paint latency: {latency} window_visible={window_visible}"
        );
        self.frames = 0;
        self.wakes = 0;
        self.last_bytes = bytes;
        self.last_report = now;
        self.latencies.clear();
    }
}
