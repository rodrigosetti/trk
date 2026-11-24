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

fn run_editor(e: &Env, args: &[&str], script: &str) -> std::process::Output {
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
    let editor = e.dir.path().join("type-editor.sh");
    std::fs::write(&editor, script).unwrap();
    StdCommand::new(assert_cmd::cargo::cargo_bin("trk"))
        .current_dir(e.repo())
        .env("HOME", e.dir.path().join("home"))
        .env("TRK_ACTOR", "human:alice@example.com")
        .env("TRK_NOW", "2026-11-20T10:00:00Z")
        .env_remove("TRK_DIR")
        .env_remove("TRK_FORMAT")
        .env_remove("VISUAL")
        .env("EDITOR", format!("sh {}", editor.display()))
        .env("CAPTURE", e.dir.path().join("type-editor-capture.md"))
        .env("EDITED", e.dir.path().join("type-editor-ran"))
        .arg("--json")
        .args(args)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave))
        .output()
        .unwrap()
}

fn assert_unknown_type(error: &Value, allowed: &[String]) {
    assert_eq!(error["code"], "invalid_field");
    let mut actual: Vec<String> = serde_json::from_value(error["allowed"].clone()).unwrap();
    actual.sort();
    assert_eq!(actual, allowed);
    assert!(error["hint"].as_str().unwrap().contains(".trk/types/"));
}

fn project_types(e: &Env) -> Vec<String> {
    let mut types: Vec<String> = std::fs::read_dir(e.trk_dir().join("types"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect();
    types.sort();
    types
}

#[test]
fn unknown_type_is_rejected_on_create_and_explicit_edit() {
    let e = Env::new();
    let allowed = project_types(&e);
    let error = e.err(
        &["create", "Unknown type", "--type", "not-a-project-type"],
        4,
        "invalid_field",
    );
    assert_unknown_type(&error, &allowed);
    assert_eq!(e.ok(&["list"]), json!([]));

    let id = e.create(&["Original title"]);
    let before = e.ok(&["show", &id]);
    let error = e.err(
        &[
            "edit",
            &id,
            "--type",
            "not-a-project-type",
            "--title",
            "Must not save",
        ],
        4,
        "invalid_field",
    );
    assert_unknown_type(&error, &allowed);
    assert_eq!(e.ok(&["show", &id]), before);

    std::fs::write(
        e.trk_dir().join("types/project-task.toml"),
        "name = \"project-task\"\ndescription = \"Custom project task\"\nagent = \"implementer\"\n",
    )
    .unwrap();
    let custom = e.create(&["Custom task", "--type", "project-task"]);
    assert_eq!(e.ok(&["show", &custom])["type"], "project-task");
    e.ok(&["edit", &id, "--type", "project-task"]);
    assert_eq!(e.ok(&["show", &id])["type"], "project-task");
}

#[test]
fn removed_type_remains_readable_and_editable_without_changing_type() {
    let e = Env::new();
    let id = e.create(&["Legacy issue", "--type", "research"]);
    std::fs::remove_file(e.trk_dir().join("types/research.toml")).unwrap();
    assert_eq!(e.ok(&["show", &id])["type"], "research");
    let listed = e.ok(&["list"]);
    assert_eq!(listed[0]["id"], id);
    assert_eq!(listed[0]["type"], "research");
    e.ok(&["edit", &id, "--title", "Retitled legacy issue"]);
    assert_eq!(e.ok(&["show", &id])["title"], "Retitled legacy issue");

    let output = run_editor(
        &e,
        &["edit", &id],
        "if [ -e \"$EDITED\" ]; then exit 1; fi\ntouch \"$EDITED\"\nsed 's/Retitled legacy issue/Editor legacy title/' \"$1\" > \"$1.next\"\nmv \"$1.next\" \"$1\"\n",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let issue = e.ok(&["show", &id]);
    assert_eq!(issue["title"], "Editor legacy title");
    assert_eq!(issue["type"], "research");
    let error = e.err(&["edit", &id, "--type", "research"], 4, "invalid_field");
    assert_unknown_type(&error, &project_types(&e));
}

#[test]
fn editor_create_and_edit_reject_changed_unknown_type() {
    for creating in [true, false] {
        let e = Env::new();
        let id = if creating {
            None
        } else {
            Some(e.create(&["Original title"]))
        };
        let before = id.as_ref().map(|id| e.ok(&["show", id]));
        let args = match id.as_deref() {
            Some(id) => vec!["edit", id],
            None => vec!["create"],
        };
        let output = run_editor(
            &e,
            &args,
            "if [ -e \"$EDITED\" ]; then cp \"$1\" \"$CAPTURE\"; exit 1; fi\ntouch \"$EDITED\"\nsed -e 's/^title    = .*/title    = \"Unknown editor type\"/' -e 's/^type     = .*/type     = \"not-a-project-type\"/' \"$1\" > \"$1.next\"\nmv \"$1.next\" \"$1\"\n",
        );
        assert!(!output.status.success());
        let retry = std::fs::read_to_string(e.dir.path().join("type-editor-capture.md"))
            .expect("invalid editor type should reopen the editor with validation errors");
        assert!(retry.contains("# error:"), "{retry}");
        assert!(retry.contains("not-a-project-type"), "{retry}");
        assert!(retry.contains(".trk/types/"), "{retry}");
        match id {
            Some(id) => assert_eq!(e.ok(&["show", &id]), before.unwrap()),
            None => assert_eq!(e.ok(&["list"]), json!([])),
        }
    }
}
