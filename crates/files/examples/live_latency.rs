//! Measures write → DiffUpdate latency of `LiveDiff`.
//!
//! `cargo run -p hangar-files --release --example live_latency -- [repo] [base] [iterations]`
//! Without `repo`, a small temporary repository is used. Probe files are untracked
//! `hangar-latency-probe-*.txt` files at the repository root, deleted afterwards.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use hangar_files::{DiffUpdate, LiveDiff, LiveDiffConfig};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

type Updates = UnboundedReceiver<hangar_files::Result<DiffUpdate>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let repo_arg = args.next();
    let base = args.next().unwrap_or_else(|| "HEAD".into());
    let iterations: usize = args.next().map(|n| n.parse()).transpose()?.unwrap_or(20);

    let _tmp;
    let repo = match repo_arg {
        Some(path) => PathBuf::from(path).canonicalize()?,
        None => {
            _tmp = tempfile::TempDir::new()?;
            let repo = _tmp.path().canonicalize()?;
            init_repo(&repo)?;
            repo
        }
    };

    let started = Instant::now();
    let (live, mut rx) = LiveDiff::start(&repo, base, LiveDiffConfig::default())?;
    let initial = next(&mut rx).await?;
    println!(
        "initial full diff: {:?} ({} files changed vs base)",
        started.elapsed(),
        initial.summary.files
    );

    // Let FSEvents settle before measuring.
    while let Ok(Some(_)) = timeout(Duration::from_millis(500), rx.recv()).await {}

    let mut add = Vec::new();
    let mut remove = Vec::new();
    for i in 0..iterations {
        let rel = PathBuf::from(format!("hangar-latency-probe-{i}.txt"));
        let path = repo.join(&rel);

        let t = Instant::now();
        fs::write(&path, format!("probe {i}\n"))?;
        wait_for(&mut rx, |u| u.changed.iter().any(|f| f.path == rel)).await?;
        add.push(t.elapsed());

        let t = Instant::now();
        fs::remove_file(&path)?;
        wait_for(&mut rx, |u| u.removed.contains(&rel)).await?;
        remove.push(t.elapsed());
    }
    report("write  → DiffUpdate", &mut add);
    report("delete → DiffUpdate", &mut remove);

    drop(live);

    // Cost of the git calls behind one update, averaged.
    let probe = [PathBuf::from("hangar-latency-probe-0.txt")];
    let rev = hangar_worktree::merge_base(&repo, "HEAD")?;
    let opts = hangar_worktree::DiffOptions::default();
    time("check_ignored (1 path)", || {
        hangar_worktree::check_ignored(&repo, &probe).map(drop)
    })?;
    time("status", || hangar_worktree::status(&repo).map(drop))?;
    time("status_paths (1 path)", || {
        hangar_worktree::status_paths(&repo, &probe).map(drop)
    })?;
    time("diff_paths (1 path)", || {
        hangar_worktree::diff_paths(&repo, &rev, &probe, &opts).map(drop)
    })?;
    time("full diff_paths", || {
        hangar_worktree::diff_paths(&repo, &rev, &[], &opts).map(drop)
    })?;
    Ok(())
}

async fn next(rx: &mut Updates) -> Result<DiffUpdate, Box<dyn Error>> {
    match timeout(Duration::from_secs(60), rx.recv()).await? {
        Some(update) => Ok(update?),
        None => Err("live diff stopped".into()),
    }
}

async fn wait_for(
    rx: &mut Updates,
    pred: impl Fn(&DiffUpdate) -> bool,
) -> Result<(), Box<dyn Error>> {
    loop {
        if pred(&next(rx).await?) {
            return Ok(());
        }
    }
}

fn time<E: Error + 'static>(
    label: &str,
    mut f: impl FnMut() -> Result<(), E>,
) -> Result<(), Box<dyn Error>> {
    const RUNS: u32 = 10;
    let t = Instant::now();
    for _ in 0..RUNS {
        f()?;
    }
    println!("{label}: {:?} avg", t.elapsed() / RUNS);
    Ok(())
}

fn report(label: &str, samples: &mut [Duration]) {
    samples.sort();
    let pick = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    println!(
        "{label}: n={} min {:?} p50 {:?} p90 {:?} max {:?}",
        samples.len(),
        pick(0.0),
        pick(0.5),
        pick(0.9),
        pick(1.0)
    );
}

fn init_repo(repo: &Path) -> Result<(), Box<dyn Error>> {
    let git = |args: &[&str]| -> Result<(), Box<dyn Error>> {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()?;
        if !status.success() {
            return Err(format!("git {args:?} failed").into());
        }
        Ok(())
    };
    git(&["init", "-q", "-b", "main"])?;
    for i in 0..50 {
        fs::write(repo.join(format!("file{i}.txt")), format!("line {i}\n"))?;
    }
    git(&["add", "-A"])?;
    git(&[
        "-c",
        "user.name=Hangar",
        "-c",
        "user.email=hangar@invalid",
        "commit",
        "-q",
        "--no-verify",
        "-m",
        "init",
    ])
}
