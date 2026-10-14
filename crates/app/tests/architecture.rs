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
    ("hangar-worktree", &[]),
    ("hangar-terminal", &[]),
    ("hangar-devices", &[]),
    ("hangar-agents", &["hangar-terminal"]),
    ("hangar-files", &["hangar-worktree"]),
    ("hangar-runner", &["hangar-devices"]),
    ("hangar-mcp", &["hangar-devices"]),
];

const OUTER: &[&str] = &["hangar-app", "hangar-examples"];

/// The UI toolkit: only outer crates may use it.
const UI: &[&str] = &["gpui-kit", "gpui", "gpui-pre", "gpui-component"];

/// External crates the domain crate may use. `rusqlite` goes once the store leaves `core`.
const CORE_EXTERNAL: &[&str] = &[
    "rusqlite",
    "serde",
    "serde_json",
    "thiserror",
    "tracing",
    "uuid",
];

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
