//! The dependency rule of docs/ARCHITECTURE.md, checked against `cargo metadata`: which
//! workspace crates each crate may use, and which external crates the inner ones may not.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Workspace crates each library crate may depend on (normal dependencies). Outer crates
/// (`hangar-app`, `hangar-examples`) are the composition root and may use any of them.
const ALLOWED: &[(&str, &[&str])] = &[
    ("hangar-core", &[]),
    ("hangar-store-sqlite", &["hangar-core"]),
    ("hangar-usecases", &["hangar-core"]),
    ("hangar-worktree", &["hangar-core"]),
    ("hangar-terminal", &[]),
    ("hangar-devices", &["hangar-core"]),
    ("hangar-agents", &[]),
    ("hangar-files", &["hangar-core"]),
    ("hangar-runner", &["hangar-core"]),
    ("hangar-mcp", &["hangar-core"]),
];

const OUTER: &[&str] = &["hangar-app", "hangar-examples"];

/// The UI toolkit: only outer crates may use it.
const UI: &[&str] = &["gpui-kit", "gpui", "gpui-pre", "gpui-component"];

/// External crates the domain crate may use: data and error derives only, no I/O or runtime.
const CORE_EXTERNAL: &[&str] = &["serde", "thiserror", "uuid"];

struct Package {
    name: String,
    deps: BTreeSet<String>,
}

fn workspace() -> Vec<Package> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| Package {
            name: p["name"].as_str().unwrap().to_owned(),
            deps: p["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["kind"].is_null())
                .map(|d| d["name"].as_str().unwrap().to_owned())
                .collect(),
        })
        .collect()
}

#[test]
fn every_crate_has_a_layer() {
    let rules: BTreeMap<_, _> = ALLOWED.iter().copied().collect();
    for package in workspace() {
        assert!(
            rules.contains_key(package.name.as_str()) || OUTER.contains(&package.name.as_str()),
            "{} has no entry in ALLOWED or OUTER: decide its layer in docs/ARCHITECTURE.md",
            package.name
        );
    }
}

#[test]
fn library_crates_only_use_allowed_workspace_crates() {
    let rules: BTreeMap<_, _> = ALLOWED.iter().copied().collect();
    for package in workspace() {
        let Some(allowed) = rules.get(package.name.as_str()) else {
            continue;
        };
        for dep in package.deps.iter().filter(|d| d.starts_with("hangar-")) {
            assert!(
                allowed.contains(&dep.as_str()),
                "{} must not depend on {dep} (see docs/ARCHITECTURE.md)",
                package.name
            );
        }
    }
}

/// Adapters meet only through `core`'s ports: no edge between two adapter crates.
#[test]
fn library_crates_depend_on_core_only() {
    for (name, allowed) in ALLOWED {
        assert!(
            allowed.iter().all(|dep| *dep == "hangar-core"),
            "{name} may only depend on hangar-core, not {allowed:?}"
        );
    }
}

#[test]
fn only_outer_crates_use_the_ui_toolkit_or_anyhow() {
    for package in workspace() {
        if OUTER.contains(&package.name.as_str()) {
            continue;
        }
        for dep in &package.deps {
            assert!(
                !UI.contains(&dep.as_str()),
                "{} is headless and must not depend on {dep}",
                package.name
            );
            assert!(
                dep != "anyhow",
                "{} is a library: use thiserror, not anyhow",
                package.name
            );
        }
    }
}

#[test]
fn core_stays_free_of_runtime_and_io_crates() {
    let core = workspace()
        .into_iter()
        .find(|p| p.name == "hangar-core")
        .unwrap();
    for dep in &core.deps {
        assert!(
            CORE_EXTERNAL.contains(&dep.as_str()),
            "hangar-core must not depend on {dep}"
        );
    }
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Test-only modules: `tests.rs` and the `fixtures.rs` beside it.
fn is_test_file(rel: &str) -> bool {
    rel.ends_with("tests.rs") || rel.ends_with("fixtures.rs")
}

/// A module's unit tests sit in it (inline or one `foo/tests.rs`), never in a `tests/` directory
/// under `src/`: too many tests for one file means the module wants splitting.
#[test]
fn unit_tests_sit_beside_their_module() {
    fn dirs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "tests") {
                    out.push(path.clone());
                }
                dirs(&path, out);
            }
        }
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            dirs(&src, &mut offenders);
        }
    }
    assert!(
        offenders.is_empty(),
        "move these tests into the module they cover (inline or `foo/tests.rs`):\n{offenders:#?}"
    );
}

/// The app names adapter crates only in `app::adapters`: everything else goes through ports, core
/// types or the re-exports there (tests aside).
#[test]
fn app_names_adapter_crates_only_in_adapters() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if rel == "adapters.rs" || rel.starts_with("adapters/") || is_test_file(&rel) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (n, line) in code.lines().enumerate() {
            if line.contains("hangar_devices::") || line.contains("hangar_worktree::") {
                offenders.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "adapter crates may be named only in app::adapters; use hangar_core::device or the \
         re-exports in adapters::devices and adapters::vcs:\n{}",
        offenders.join("\n")
    );
}

/// Views (the shell and the panels) read and write settings and the usage ledger through
/// `app::ops`, on the background executor; they call no such store method themselves (tests
/// aside).
#[test]
fn views_reach_the_store_through_ops() {
    const VIEWS: [&str; 6] = [
        "shell",
        "chat_view",
        "changes",
        "device_panel",
        "device_view",
        "files_view",
    ];
    const CALLS: [&str; 6] = [
        ".get_setting(",
        ".set_setting(",
        ".delete_setting(",
        ".record_usage(",
        ".record_session_cost(",
        "SettingsExt",
    ];
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let is_view = VIEWS
            .iter()
            .any(|v| rel == format!("{v}.rs") || rel.starts_with(&format!("{v}/")));
        if !is_view || is_test_file(&rel) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (n, line) in code.lines().enumerate() {
            if CALLS.iter().any(|c| line.contains(c)) {
                offenders.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "views go through app::ops for settings and the usage ledger:\n{}",
        offenders.join("\n")
    );
}
