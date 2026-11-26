use super::*;
use std::io::Read;
use std::os::fd::FromRawFd;

/// A real stdout terminal without borrowing the user's terminal or check queue.
struct Terminal {
    reader: std::fs::File,
    writer: std::fs::File,
}

impl Terminal {
    fn new() -> Self {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty initializes both descriptors on success; no termios or
        // window-size overrides are needed. Each File takes sole ownership.
        unsafe {
            assert_eq!(
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut::<libc::termios>(),
                    std::ptr::null_mut::<libc::winsize>(),
                ),
                0
            );
            let reader = std::fs::File::from_raw_fd(master);
            let writer = std::fs::File::from_raw_fd(slave);
            assert_eq!(libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK), 0);
            Self { reader, writer }
        }
    }

    fn command(&self, e: &Env, args: &[&str]) -> StdCommand {
        let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
        command
            .current_dir(e.repo())
            .env("HOME", e.dir.path().join("home"))
            .env("TRK_USER_DIR", e.user_dir())
            .env("TRK_ACTOR", "human:alice@example.com")
            .env("TRK_NOW", "2026-11-20T10:00:00Z")
            .env_remove("TRK_DIR")
            .env_remove("TRK_FORMAT")
            .env_remove("NO_COLOR")
            .stdout(self.writer.try_clone().unwrap())
            .stderr(Stdio::piped())
            .args(args);
        command
    }

    fn drain(&mut self, text: &mut String) {
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            match self.reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(e) => panic!("reading terminal: {e}"),
            }
        }
        text.push_str(&String::from_utf8(bytes).unwrap());
    }

    fn run(&mut self, e: &Env, args: &[&str], no_color: Option<&str>) -> String {
        let mut command = self.command(e, args);
        if let Some(value) = no_color {
            command.env("NO_COLOR", value);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let mut text = String::new();
        self.drain(&mut text);
        text
    }
}

#[test]
fn check_colors_terminal_results_and_plain_payloads() {
    let e = Env::new();
    let passed = e.waiting("successful check", "echo child-success");
    let retry = e.waiting("pending check", "echo child-wait; exit 75");
    let failed = e.waiting("broken check", "echo child-failure; exit 3");
    let mut terminal = Terminal::new();
    let text = terminal.run(&e, &["check", "--no-gc"], None);
    for (id, title) in [
        (&passed, "successful check"),
        (&retry, "pending check"),
        (&failed, "broken check"),
    ] {
        assert!(text.contains(id.as_str()), "{text:?}");
        assert!(text.contains(title), "{text:?}");
    }
    assert!(text.contains("\x1b[1m"), "{text:?}");
    assert!(text.contains("\x1b[36m"), "{text:?}");
    assert!(text.contains("\x1b[32mpassed → closed"), "{text:?}");
    assert!(text.contains("\x1b[36mnot yet (exit 75)"), "{text:?}");
    assert!(
        text.contains("\x1b[31m") && text.contains("failed (exit 3)"),
        "{text:?}"
    );
    assert!(
        !text.contains("child-"),
        "subprocess output leaked: {text:?}"
    );

    let command = "printf 'uncolored payload'; exit 75";
    e.ok(&["wait", &retry, "--check", command]);
    let preview = terminal.run(&e, &["check", &retry, "--dry-run"], None);
    assert!(preview.contains("\x1b[36mcheck"), "{preview:?}");
    assert!(preview.contains(command), "{preview:?}");
}

#[test]
fn check_colors_respect_disable_and_machine_formats() {
    let e = Env::new();
    let id = e.waiting("pending check", "echo plain-output; exit 75");
    let mut terminal = Terminal::new();
    for no_color in [Some("1"), Some("")] {
        let text = terminal.run(&e, &["check", &id], no_color);
        assert!(text.contains("not yet (exit 75)"), "{text:?}");
        assert!(!text.contains('\x1b'), "{text:?}");
    }
    let text = terminal.run(&e, &["--no-color", "check", &id], None);
    assert!(text.contains("not yet (exit 75)"), "{text:?}");
    assert!(!text.contains('\x1b'), "{text:?}");

    for args in [
        vec!["--json"],
        vec!["--format", "json"],
        vec!["--format", "jsonl"],
    ] {
        let mut args = args;
        args.extend(["check", &id]);
        let text = terminal.run(&e, &args, None);
        assert!(!text.contains('\x1b'), "{text:?}");
        let value: Value = serde_json::from_str(&text).unwrap();
        let data = if value.get("ok").is_some() {
            &value["data"]
        } else {
            &value
        };
        assert_eq!(data["results"][0]["issue"], id);
        assert_eq!(data["results"][0]["result"], "retry");
    }
    let output = e
        .cmd()
        .env_remove("NO_COLOR")
        .args(["--format", "pretty", "check", &id])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(text.contains("not yet (exit 75)"), "{text:?}");
    assert!(!text.contains('\x1b'), "{text:?}");
}

#[test]
fn check_colors_watch_uses_terminal_policy() {
    let e = Env::new();
    let id = e.waiting(
        "watched check",
        "echo $$ > \"$TRK_REPO/check-shell\"; exit 75",
    );
    for (args, colored) in [
        (vec!["check", &id, "--watch", "1h"], true),
        (vec!["--no-color", "check", &id, "--watch", "1h"], false),
        (vec!["--json", "check", &id, "--watch", "1h"], false),
    ] {
        let mut terminal = Terminal::new();
        let mut runner = CheckRunner::spawn_command(&e, terminal.command(&e, &args));
        let mut text = String::new();
        check_wait_until("the watch result to reach the terminal", || {
            terminal.drain(&mut text);
            text.contains("not yet (exit 75)")
        });
        runner.signal("-INT");
        assert!(!runner.wait().success());
        assert_eq!(text.contains('\x1b'), colored, "{text:?}");
        assert!(text.contains(&id), "{text:?}");
    }
}
