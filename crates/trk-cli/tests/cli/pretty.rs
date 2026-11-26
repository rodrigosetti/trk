use super::*;

#[test]
fn pretty_output() {
    let e = Env::new();
    let a = e.waiting("Add claim leases", "git merge --no-edit no-such-branch");
    let b = e.create(&["Enable leases", "--blocked-by", &a]);
    e.ok(&[
        "claim",
        &e.create(&["In flight"]),
        "--as",
        "agent:omp:12938192381",
    ]);
    let human = e.create(&["Pick a store"]);
    e.ok(&["claim", &human]);
    e.ok(&["wait", &human, "-r", "SQLite or Dolt?"]);
    let out = e
        .cmd()
        .args(["--format", "pretty", "list", "--sort", "created"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("ID"), "{text}");
    assert!(text.contains("◷ waiting"), "{text}");
    assert!(text.contains("testing · check now"), "{text}");
    assert!(text.contains("⛔ blocked (0/1 done)"), "{text}");
    assert!(text.contains("● claimed"), "{text}");
    assert!(text.contains("omp:1293…"), "{text}");
    assert!(text.contains("SQLite or Dolt? · needs a human"), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "show", &a])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    // The check is shown in full: it runs unattended.
    assert!(
        text.contains("check  git merge --no-edit no-such-branch"),
        "{text}"
    );
    assert!(text.contains("every  5m"), "{text}");
    assert!(text.contains("pass   closes the issue"), "{text}");
    assert!(text.contains(&format!("blocks     {b}")), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "show", &human])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("no check: a human resolves it"), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "check"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains(&format!("✗ {a}  failed")), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "log", &a])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("waiting for testing; check every 5m: git merge"),
        "{text}"
    );
    assert!(text.contains("check failed (exit"), "{text}");
    assert!(
        text.contains("reopened from waiting by its check"),
        "{text}"
    );
    let out = e
        .cmd()
        .args(["--format", "pretty", "close", &b, "--reason", "nope"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("error: invalid close reason 'nope'"), "{err}");
    assert!(err.contains("allowed: done, abandoned"), "{err}");
}

#[test]
fn checked_wait_surfaces_describe_the_pass_action_without_color() {
    for (policy, action) in [("close", "closes"), ("wake", "wakes")] {
        let e = Env::new();
        let id = e.create(&["Checked waiting surface"]);
        e.ok(&["claim", &id]);
        let waiting = e.ok(&[
            "wait",
            &id,
            "--check",
            "exit 75",
            "--on-pass",
            policy,
            "-r",
            "review pending",
        ]);
        assert_eq!(waiting["waiting"]["on_pass"], policy);
        assert_eq!(
            e.ok(&["dashboard"])["issues"][0]["waiting"]["on_pass"],
            policy
        );
        for args in [
            vec!["list", "state=waiting"],
            vec!["list", "state=waiting", "--fields", "id,waiting"],
            vec!["show", &id],
        ] {
            let out = e
                .cmd()
                .args(["--no-color", "--format", "pretty"])
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
            let text = String::from_utf8_lossy(&out.stdout);
            let expected = if args[0] == "show" {
                format!("pass   {action}")
            } else {
                format!("pass {action}")
            };
            assert!(text.contains(&expected), "{args:?}: {text}");
            assert!(!text.contains('\u{1b}'), "{text}");
        }
    }
}

#[test]
fn pretty_priority_and_state_labels_are_readable_without_color() {
    let e = Env::new();
    e.create(&["-p", "P0", "open"]);
    let claimed = e.create(&["-p", "P1", "claimed"]);
    e.ok(&["claim", &claimed]);
    let waiting = e.create(&["-p", "P2", "waiting"]);
    e.ok(&["claim", &waiting]);
    e.ok(&["wait", &waiting, "--reason", "later"]);
    let closed = e.create(&["-p", "P3", "closed"]);
    e.ok(&["close", &closed]);

    let pretty_output = || {
        e.cmd()
            .args(["--format", "pretty", "list"])
            .output()
            .unwrap()
    };
    let out = pretty_output();
    assert!(out.status.success());
    let pretty = String::from_utf8(out.stdout).unwrap();
    assert!(
        pretty.contains("P0") && pretty.contains("○ open"),
        "{pretty}"
    );
    assert!(
        pretty.contains("P1") && pretty.contains("● claimed"),
        "{pretty}"
    );
    assert!(
        pretty.contains("P2") && pretty.contains("◷ waiting"),
        "{pretty}"
    );
    assert!(
        pretty.contains("P3") && pretty.contains("✓ closed"),
        "{pretty}"
    );
    assert!(!pretty.contains('\x1b'), "{pretty:?}");

    let no_color = e
        .cmd()
        .env("NO_COLOR", "1")
        .args(["--format", "pretty", "list"])
        .output()
        .unwrap();
    assert!(no_color.status.success());
    assert!(!no_color.stdout.contains(&0x1b));

    let json = e.cmd().args(["--json", "ready"]).output().unwrap();
    assert!(json.status.success());
    assert!(!json.stdout.contains(&0x1b));
    serde_json::from_slice::<Value>(&json.stdout).unwrap();
}
