use super::*;

#[test]
fn run_print_creates_private_session_and_harness_files() {
    use std::os::unix::fs::PermissionsExt;

    let e = Env::new();
    let config = e.trk_dir().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        config,
        format!(
            "{text}\n\
             [harness.private-files]\n\
             command = 'sh'\n\
             builtin_tools = {{ read-only = [] }}\n\
             [harness.private-files.files]\n\
             '.generated/nested/new.json' = '{{{{ mcp_servers | tojson }}}}'\n\
             '.generated/existing.json' = '{{{{ actor }}}}'\n\
             '{{{{ session_dir }}}}/nested/profile.json' = '{{{{ actor }}}}'\n\
             '{{{{ cwd }}}}/absolute/nested/profile.json' = '{{{{ actor }}}}'\n"
        ),
    )
    .unwrap();
    let existing = e.repo().join(".generated/existing.json");
    std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
    std::fs::write(
        &existing,
        "old public contents that must be replaced completely",
    )
    .unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o666)).unwrap();
    let id = e.create(&["Private generated files", "--type", "research"]);

    // Set the umask only in the child, independent of concurrent tests and the host.
    let out = StdCommand::new("sh")
        .args(["-c", "umask 000; exec \"$@\"", "sh"])
        .arg(env!("CARGO_BIN_EXE_trk"))
        .args([
            "--json",
            "run",
            &id,
            "--harness",
            "private-files",
            "--print",
        ])
        .current_dir(e.repo())
        .env("TRK_USER_DIR", e.user_dir())
        .env("HOME", e.dir.path().join("home"))
        .env("TRK_ACTOR", "human:alice@example.com")
        .env("TRK_NOW", "2026-11-20T10:00:00Z")
        .env_remove("TRK_FORMAT")
        .env_remove("TRK_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["ok"], true, "{envelope:#}");
    let spec = &envelope["data"];
    let session = PathBuf::from(spec["session_dir"].as_str().unwrap());
    for dir in [
        e.trk_dir().join("sessions"),
        session.clone(),
        session.join("nested"),
        e.repo().join(".generated/nested"),
        e.repo().join("absolute"),
        e.repo().join("absolute/nested"),
    ] {
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777,
            0o700,
            "{}",
            dir.display()
        );
    }
    for name in ["briefing.md", "instructions.md", "mcp.json"] {
        assert!(session.join(name).is_file(), "{name}");
    }
    let files = spec["files"].as_object().unwrap();
    assert_eq!(files.len(), 7);
    for (path, content) in files {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o7777,
            0o600,
            "{path}"
        );
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            content.as_str().unwrap(),
            "{path}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(existing).unwrap(),
        spec["actor"].as_str().unwrap()
    );
}
