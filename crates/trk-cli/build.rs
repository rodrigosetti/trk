//! Embed the git commit in `trk --version`, so an installed binary says
//! which checkout it was built from.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let pkg = env!("CARGO_PKG_VERSION");
    let version = match git(&["rev-parse", "--short=10", "HEAD"]) {
        Some(c) => format!("{pkg} ({c})"),
        None => pkg.to_string(),
    };
    println!("cargo:rustc-env=TRK_VERSION={version}");
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for p in ["HEAD", "refs/heads", "packed-refs"] {
            println!("cargo:rerun-if-changed={dir}/{p}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
