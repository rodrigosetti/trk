use super::*;

#[test]
fn issue_agent_flags_are_rejected() {
    let e = Env::new();
    let id = e.create(&["Type-only issue"]);
    for args in [
        vec!["create", "Old flag", "--agent", "implementer"],
        vec!["edit", &id, "--agent", "implementer"],
    ] {
        let out = e.cmd().args(&args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--agent"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn deps_and_imported_ids() {
    let e = Env::new();
    let a = e.create(&["A", "--id", "orch-aaa"]);
    assert_eq!(a, "orch-aaa");
    let b = e.create(&["B"]);
    let c = e.create(&["C", "--blocks", &a]);
    assert_eq!(e.ok(&["dep", "add", "aaa", &b])["added"], true);
    assert_eq!(e.ok(&["dep", "add", &a, &b])["added"], false);
    e.err(&["dep", "add", &b, &c], 4, "cycle");
    assert_eq!(
        e.ok(&["list", &format!("blocked-by={a}")])[0]["id"],
        json!(b)
    );
    assert_eq!(e.ok(&["list", "blocked"]).as_array().unwrap().len(), 2);
    e.ok(&["dep", "rm", &a, &b]);
    e.err(&["dep", "rm", &a, &b], 3, "not_found");
    e.err(&["create", "X", "--id", "other-1"], 2, "bad_args");
    e.err(&["create", "X", "--id", "orch-aaa"], 2, "bad_args");
}

#[test]
fn link_and_unlink_show_on_the_issue_and_the_dashboard_snapshot() {
    let e = Env::new();
    let id = e.create(&["Ship it"]);
    let pr = "https://example.com/org/repo/pull/12";
    let linked = e.ok(&["link", &id, pr, "-l", "PR #12"]);
    assert_eq!(linked["changed"], true);
    assert_eq!(e.ok(&["link", &id, pr, "-l", "PR #12"])["changed"], false);
    assert_eq!(
        e.ok(&["show", &id])["links"],
        json!([{"url": pr, "label": "PR #12"}])
    );
    let dash = e.ok(&["dashboard"]);
    assert_eq!(dash["issues"][0]["links"][0]["label"], "PR #12");
    e.err(&["link", &id, "not a url"], 4, "invalid_field");

    assert_eq!(e.ok(&["unlink", &id, pr])["links"], json!([]));
    assert!(e.ok(&["show", &id]).get("links").is_none());
    e.err(&["unlink", &id, pr], 3, "not_found");
}

#[test]
fn errors_and_exit_codes() {
    let e = Env::new();
    let id = e.create(&["Thing"]);
    e.err(&["show", "nope-zzz"], 3, "not_found");
    e.err(&["list", "bogus"], 2, "bad_query");
    e.err(&["list", "agent=implementer"], 2, "bad_query");
    e.err(&["create", "X", "-p", "P7"], 4, "invalid_field");
    e.err(&["create", "X", "--type", ""], 4, "invalid_field");
    e.err(&["edit", &id, "--type", ""], 4, "invalid_field");
    e.err(&["close", &id, "--reason", "merged"], 4, "invalid_field");
    e.err(&["unclaim", &id], 4, "invalid_state");
    e.err(&["reopen", &id], 4, "invalid_state");
    e.err(
        &["wait", &id, "-c", "true", "--every", "soon"],
        2,
        "bad_args",
    );
    e.ok(&["claim", &id, "--as", "agent:omp:s1"]);
    let err = e.err(&["claim", &id], 5, "claimed_by_other");
    assert_eq!(err["claimant"], "agent:omp:s1");
    let err = e.err(&["close", &id], 5, "not_claimant");
    assert!(!err["hint"].as_str().unwrap().contains("trk unclaim"));
    let err = e.err(
        &["unclaim", id.strip_prefix("orch-").unwrap()],
        5,
        "not_claimant",
    );
    assert_eq!(err["issue"], id);
    assert_eq!(err["claimant"], "agent:omp:s1");
    assert!(
        err["hint"]
            .as_str()
            .unwrap()
            .contains(&format!("trk unclaim {id} --force"))
    );
    let err = e.err(
        &["unclaim", &id, "--force", "--as", "agent:omp:s2"],
        5,
        "not_permitted",
    );
    assert!(err.get("hint").is_none());
    assert_eq!(e.ok(&["show", &id])["claim"]["actor"], "agent:omp:s1");
    e.ok(&["unclaim", &id, "--force"]);
    assert_eq!(e.ok(&["show", &id])["state"], "open");
    let log = e.ok(&["log", &id]);
    let release = log
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["op"] == "release")
        .unwrap();
    assert_eq!(release["payload"]["forced"], true);
    e.ok(&["claim", &id, "--as", "agent:omp:s1"]);
    e.err(
        &["note", &id, "x", "--as", "agent:omp:s2"],
        5,
        "not_permitted",
    );
    e.err(
        &["create", "Stray", "--as", "agent:omp:s1"],
        5,
        "not_permitted",
    );
    // --force is recorded.
    e.ok(&["close", &id, "--force"]);
    let log = e.ok(&["log", &id]);
    let close = log
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["op"] == "close")
        .unwrap();
    assert_eq!(close["payload"]["forced"], true);
    // --dry-run changes nothing.
    let other = e.create(&["Other"]);
    assert_eq!(e.ok(&["close", &other, "--dry-run"])["state"], "closed");
    assert_eq!(e.ok(&["show", &other])["state"], "open");
    // Ambiguous id prefixes are refused.
    e.create(&["P", "--id", "orch-ab1"]);
    e.create(&["Q", "--id", "orch-ab2"]);
    let err = e.err(&["show", "ab"], 3, "ambiguous_id");
    assert_eq!(err["candidates"].as_array().unwrap().len(), 2);
    // Without --json on a non-TTY the output is still JSON.
    let out = e.cmd().args(["show", "nope-zzz"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
}
