use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn isolated() -> (Env, PathBuf) {
    let e = Env::with_install(true);
    std::fs::remove_file(e.trk_dir().join("config.toml")).unwrap();
    let bin = e.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    for (name, target) in [("git", "/usr/bin/git"), ("sh", "/bin/sh")] {
        symlink(target, bin.join(name)).unwrap();
    }
    (e, bin)
}

fn executable(bin: &Path, name: &str, body: &str) {
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn install(e: &Env, bin: &Path, args: &[&str]) -> Value {
    let mut command = e.cmd();
    command.env("PATH", bin);
    resume::success(command, args)
}

#[test]
fn install_detects_seed_profiles_in_order_and_warns_for_each_missing_category() {
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["herdr", "hc", "tmux"],
            &["omp", "claude-code", "codex", "opencode", "pi"],
        ),
        (&["tmux"], &["codex", "pi"]),
        (&[], &[]),
        (&[], &["omp"]),
        (&["hc"], &[]),
    ];
    for preset in ["local", "github"] {
        for &(launchers, harnesses) in cases {
            let (e, bin) = isolated();
            // Installed but unusable launchers must not enter the selection.
            executable(&bin, "herdr-app", "printf 'not JSON\\n'");
            executable(&bin, "hc", "exit 7");
            executable(&bin, "tmux", "exit 7");
            for launcher in launchers.iter().rev() {
                let (command, body) = match *launcher {
                    "herdr" => (
                        "herdr-app",
                        "[ \"$1\" = ls ] && printf '{\"spaces\":[]}\\n'",
                    ),
                    "hc" => ("hc", "[ \"$1\" = ls ]"),
                    "tmux" => ("tmux", "[ \"$1\" = -V ]"),
                    _ => unreachable!(),
                };
                executable(&bin, command, body);
            }
            for harness in harnesses.iter().rev() {
                let command = if *harness == "claude-code" {
                    "claude"
                } else {
                    harness
                };
                // Detect the command without starting an agent or needing credentials.
                executable(&bin, command, "exit 91");
            }
            let result = install(&e, &bin, &["install", "--preset", preset]);
            assert_eq!(result["detected"]["launchers"], json!(launchers));
            assert_eq!(result["detected"]["harnesses"], json!(harnesses));
            let config: toml::Value =
                toml::from_str(&std::fs::read_to_string(e.trk_dir().join("config.toml")).unwrap())
                    .unwrap();
            assert_eq!(
                serde_json::to_value(&config["launcher"]["use"]).unwrap(),
                json!(launchers)
            );
            assert_eq!(
                serde_json::to_value(&config["harness"]["prefer"]).unwrap(),
                json!(harnesses)
            );
            let warnings = result["warnings"].as_array().unwrap();
            for (category, missing) in [
                ("launcher", launchers.is_empty()),
                ("harness", harnesses.is_empty()),
            ] {
                assert_eq!(
                    warnings.iter().any(|w| w
                        .as_str()
                        .unwrap()
                        .contains(&format!("no {category} detected"))),
                    missing,
                    "{result:#}"
                );
            }
        }
    }
}

#[test]
fn install_keeps_existing_preferences_until_forced() {
    let (e, bin) = isolated();
    executable(&bin, "tmux", "exit 0");
    executable(&bin, "claude", "exit 91");
    install(&e, &bin, &["install"]);
    let path = e.trk_dir().join("config.toml");
    let original = std::fs::read(&path).unwrap();
    std::fs::remove_file(bin.join("tmux")).unwrap();
    std::fs::remove_file(bin.join("claude")).unwrap();
    executable(&bin, "hc", "exit 0");
    executable(&bin, "omp", "exit 91");
    let kept = install(&e, &bin, &["install"]);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(kept["differ"], json!([]));
    assert!(kept["detected"].is_null());
    let replaced = install(&e, &bin, &["install", "--force"]);
    assert_eq!(
        replaced["detected"],
        json!({"launchers": ["hc"], "harnesses": ["omp"]})
    );
    let config: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(config["launcher"]["use"][0].as_str(), Some("hc"));
    assert_eq!(config["harness"]["prefer"][0].as_str(), Some("omp"));
}

#[test]
fn install_pretty_output_reports_missing_commands_and_recovery() {
    let (e, bin) = isolated();
    let out = e
        .cmd()
        .env("PATH", bin)
        .args(["--format", "pretty", "--no-color", "install"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    for expected in [
        "launchers none detected",
        "harnesses none detected",
        "warning: no launcher detected",
        "warning: no harness detected",
        "[launcher] use",
        "[harness] prefer",
        "trk run --fg",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn install_landing_warnings_follow_effective_config_and_available_gh() {
    let (e, bin) = isolated();
    let local = install(&e, &bin, &["install", "--preset", "local"]);
    let warnings = local["warnings"].to_string();
    assert!(
        warnings.contains("no review or approval gate"),
        "{warnings}"
    );
    assert!(warnings.contains("skip verification"), "{warnings}");
    e.set_config("verify = \"\"", "verify = \"true\"");
    let verified = install(&e, &bin, &["install"]);
    assert!(
        !verified["warnings"]
            .to_string()
            .contains("skip verification")
    );
    e.set_config("verify = \"true\"", "verify = \"   \"");
    let blank = install(&e, &bin, &["install"]);
    assert!(blank["warnings"].to_string().contains("skip verification"));
    let github = install(&e, &bin, &["install", "--preset", "github", "--force"]);
    let warnings = github["warnings"].to_string();
    assert!(warnings.contains("gh auth login"), "{warnings}");
    assert!(warnings.contains("not on PATH"), "{warnings}");
    assert!(!warnings.contains("skip verification"), "{warnings}");
    executable(&bin, "gh", "exit 91");
    let present = install(&e, &bin, &["install", "--preset", "github"]);
    let warnings = present["warnings"].to_string();
    assert!(warnings.contains("gh auth login"), "{warnings}");
    assert!(!warnings.contains("not on PATH"), "{warnings}");
}
