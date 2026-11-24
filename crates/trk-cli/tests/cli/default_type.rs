use super::*;
use std::fs::File;
use std::os::fd::FromRawFd;

#[test]
fn configured_default_type_drives_create_editor_and_whoami() {
    let e = Env::new();
    e.set_config("default_type = \"code-change\"", "default_type = \"task\"");
    std::fs::write(
        e.trk_dir().join("types/task.toml"),
        "name = \"task\"\ndescription = \"Project task\"\nagent = \"implementer\"\n",
    )
    .unwrap();
    std::fs::remove_file(e.trk_dir().join("types/code-change.toml")).unwrap();

    assert_eq!(e.ok(&["whoami"])["default_type"], "task");
    let id = e.create(&["Use the project default"]);
    let issue = e.ok(&["show", &id]);
    assert_eq!(issue["type"], "task");
    assert_eq!(issue["routing"]["agent"], "implementer");
    let explicit = e.create(&["Use an explicit type", "--type", "research"]);
    assert_eq!(e.ok(&["show", &explicit])["type"], "research");

    // Both input and output must be terminals to enter the create editor.
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                std::ptr::null_mut::<libc::winsize>(),
            )
        },
        0
    );
    let _master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let editor = e.dir.path().join("editor.sh");
    let capture = e.dir.path().join("template.md");
    std::fs::write(
        &editor,
        "cp \"$1\" \"$CAPTURE\"\nsed 's/title    = \"\"/title    = \"From editor\"/' \"$1\" > \"$1.next\"\nmv \"$1.next\" \"$1\"\n",
    )
    .unwrap();
    let output = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"))
        .current_dir(e.repo())
        .env("HOME", e.dir.path().join("home"))
        .env("TRK_USER_DIR", e.user_dir())
        .env("TRK_ACTOR", "human:alice@example.com")
        .env("TRK_NOW", "2026-11-20T10:00:00Z")
        .env_remove("TRK_DIR")
        .env_remove("TRK_FORMAT")
        .env_remove("VISUAL")
        .env("EDITOR", format!("sh {}", editor.display()))
        .env("CAPTURE", &capture)
        .args(["--json", "create"])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let template = std::fs::read_to_string(capture).unwrap();
    let front = template
        .strip_prefix("+++\n")
        .unwrap()
        .split_once("\n+++")
        .unwrap()
        .0;
    let fields: toml::Table = toml::from_str(front).unwrap();
    assert_eq!(fields["type"].as_str(), Some("task"));
    let edited = e.ok(&["list", "title=\"From editor\""]);
    assert_eq!(edited[0]["title"], "From editor");
    assert_eq!(edited[0]["type"], "task");
}

#[test]
fn older_config_keeps_the_builtin_default_type() {
    let e = Env::new();
    e.set_config("default_type = \"code-change\"\n", "");
    let id = e.create(&["Created with an older config"]);
    assert_eq!(e.ok(&["show", &id])["type"], "code-change");
    assert_eq!(e.ok(&["whoami"])["default_type"], "code-change");
}
