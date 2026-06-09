//! Build script: stamp a build datetime and git short hash into the binary so
//! the running app can show exactly which build it is (see the hl-viewer
//! footer). We deliberately emit NO `rerun-if-changed` instructions so cargo
//! falls back to its default — re-run whenever any package file changes — which
//! means a fresh datetime/hash is baked on every real rebuild.

use std::process::Command;

fn main() {
    // Human-readable UTC build time, e.g. "2026-06-09 21:55:03 UTC".
    let build_dt = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();
    println!("cargo:rustc-env=HL_BUILD_DATETIME={build_dt}");

    // Short git hash (with a "-dirty" suffix when the tree has uncommitted
    // changes). Falls back to "unknown" outside a git checkout.
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let hash = if dirty { format!("{hash}-dirty") } else { hash };
    println!("cargo:rustc-env=HL_GIT_HASH={hash}");
}
