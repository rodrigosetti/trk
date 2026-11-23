//! Store-level tests of the four-state machine.

use trk_core::ops::{CheckResult, CheckRun, ClaimOpts, Edit, NewIssue, WaitOpts};
use trk_core::view::With;
use trk_core::{Code, State, Store, StoreOpts};

const HUMAN: &str = "human:alice@example.com";
const AGENT: &str = "agent:omp:s1";
const AGENT2: &str = "agent:omp:s2";

fn ts(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
}

fn store_at(dir: &tempfile::TempDir, now: &str) -> Store {
    let opts = StoreOpts {
        now: Some(ts(now)),
        prefix: "orch".into(),
        ..Default::default()
    };
    Store::install(&dir.path().join("trk.db"), opts).unwrap()
}

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = store_at(&dir, "2026-11-20T10:00:00Z");
    (dir, s)
}

fn new(title: &str) -> NewIssue {
    NewIssue {
        title: title.into(),
        description: "why and what".into(),
        ..Default::default()
    }
}

fn create(s: &mut Store, title: &str) -> String {
    s.write(HUMAN, false, |c| c.create(new(title))).unwrap()
}

fn state(s: &Store, id: &str) -> State {
    s.read(None, |c| c.issue(id)).unwrap().state
}

fn claim(s: &mut Store, actor: &str, id: &str) {
    s.write(actor, false, |c| {
        c.claim(
            id,
            ClaimOpts {
                worktree: Some("/tmp/wt".into()),
                ..Default::default()
            },
        )
    })
    .unwrap();
}

fn wait(s: &mut Store, actor: &str, id: &str, check: Option<&str>, reason: Option<&str>) {
    s.write(actor, false, |c| {
        c.wait(
            id,
            WaitOpts {
                check: check.map(String::from),
                reason: reason.map(String::from),
                ..Default::default()
            },
        )
    })
    .unwrap();
}

fn run(exit: i32, output: &str) -> CheckRun {
    CheckRun {
        exit: Some(exit),
        timed_out: false,
        output: output.into(),
    }
}

fn record(s: &mut Store, id: &str, check: &str, r: &CheckRun) -> CheckResult {
    s.write(HUMAN, false, |c| c.check_record(id, check, r, None))
        .unwrap()
}

fn ids(s: &Store, q: &str) -> Vec<String> {
    s.read(Some(HUMAN), |c| c.list(q, None, None))
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn ops_of(s: &Store, id: &str) -> Vec<String> {
    s.read(None, |c| c.events(id))
        .unwrap()
        .into_iter()
        .map(|e| e.op)
        .collect()
}

#[test]
fn create_defaults_and_validation() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "First");
    assert!(id.starts_with("orch-"));
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Open);
    assert_eq!(i.priority, "P2");
    assert_eq!(i.issue_type, trk_core::model::DEFAULT_TYPE);
    let e = s.write(HUMAN, false, |c| c.create(new("  "))).unwrap_err();
    assert_eq!(e.code(), Code::InvalidField);
    let e = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                priority: Some("P9".into()),
                ..new("x")
            })
        })
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidField);
    // A fixed id is kept, and cannot be reused.
    let fixed = NewIssue {
        id: Some("orch-abc".into()),
        ..new("Imported")
    };
    assert_eq!(
        s.write(HUMAN, false, |c| c.create(fixed.clone())).unwrap(),
        "orch-abc"
    );
    assert!(s.write(HUMAN, false, |c| c.create(fixed.clone())).is_err());
    assert_eq!(s.read(None, |c| c.resolve_id("abc")).unwrap(), "orch-abc");
}

#[test]
fn ready_means_open_with_every_blocker_closed() {
    let (_d, mut s) = setup();
    let a = create(&mut s, "A");
    let b = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                blocked_by: vec![a.clone()],
                ..new("B")
            })
        })
        .unwrap();
    assert_eq!(ids(&s, "ready"), vec![a.clone()]);
    assert_eq!(ids(&s, "blocked"), vec![b.clone()]);
    let e = s
        .write(HUMAN, false, |c| c.claim(&b, ClaimOpts::default()))
        .unwrap_err();
    assert_eq!(e.code(), Code::Blocked);

    // A waiting blocker still blocks: its work has not landed yet.
    claim(&mut s, HUMAN, &a);
    wait(&mut s, HUMAN, &a, Some("true"), None);
    assert!(ids(&s, "ready").is_empty());
    let v = s.read(None, |c| c.show(&b, With::default())).unwrap();
    assert_eq!(v["derived"]["ready"], false);
    assert_eq!(v["derived"]["blocked_by"][0], a.as_str());
    assert_eq!(v["derived"]["progress"]["closed"], 0);

    assert_eq!(record(&mut s, &a, "true", &run(0, "")), CheckResult::Passed);
    assert_eq!(ids(&s, "ready"), vec![b.clone()]);
    let v = s.read(None, |c| c.show(&b, With::default())).unwrap();
    assert_eq!(v["derived"]["progress"]["closed"], 1);
}

#[test]
fn ready_is_ordered_by_priority_then_age() {
    let (_d, mut s) = setup();
    let old = create(&mut s, "old");
    s.opts.now = Some(ts("2026-11-20T11:00:00Z"));
    let recent = create(&mut s, "recent");
    s.opts.now = Some(ts("2026-11-20T12:00:00Z"));
    let urgent = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                priority: Some("p0".into()),
                ..new("urgent")
            })
        })
        .unwrap();
    assert_eq!(ids(&s, "ready"), vec![urgent.clone(), old, recent.clone()]);
    assert_eq!(ids(&s, "priority=p0"), vec![urgent]);
    let ch = s
        .write(HUMAN, false, |c| {
            c.edit(
                &recent,
                Edit {
                    priority: Some("P1".into()),
                    title: Some("recent".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(ch.len(), 1);
    assert_eq!(ch["priority"]["to"], "P1");
    assert_eq!(ids(&s, "ready")[1], recent);
}

#[test]
fn claim_close_and_claimant_rules() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    claim(&mut s, AGENT, &id);
    assert_eq!(state(&s, &id), State::Claimed);
    assert!(ids(&s, "ready").is_empty());
    let e = s
        .write(AGENT2, false, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap_err();
    assert_eq!(e.code(), Code::ClaimedByOther);
    // Agents write only to what they claim.
    let e = s
        .write(AGENT2, false, |c| c.note(&id, "hi", None, false))
        .unwrap_err();
    assert_eq!(e.code(), Code::NotPermitted);
    // A human needs --force to close someone else's claim.
    let e = s
        .write(HUMAN, false, |c| c.close(&id, "done", None, false))
        .unwrap_err();
    assert_eq!(e.code(), Code::NotClaimant);
    let e = s
        .write(AGENT, false, |c| c.close(&id, "merged", None, false))
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidField);
    s.write(AGENT, false, |c| {
        c.close(&id, "done", Some("shipped"), false)
    })
    .unwrap();
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Closed);
    assert_eq!(i.close_reason.as_deref(), Some("done"));
    assert!(s.read(None, |c| c.active_claim(&id)).unwrap().is_none());
    let claims = s.read(None, |c| c.claims(&id)).unwrap();
    assert_eq!(claims[0].release_reason.as_deref(), Some("closed"));
    assert_eq!(ids(&s, "reason=done"), vec![id.clone()]);
    let e = s
        .write(HUMAN, false, |c| c.close(&id, "done", None, false))
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidState);
    // Reopen brings it back.
    s.write(HUMAN, false, |c| c.reopen(&id, Some("not actually done")))
        .unwrap();
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Open);
    assert_eq!(i.close_reason, None);
    assert_eq!(i.closed_at, None);
}

#[test]
fn unclaim_steal_and_cancel() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    claim(&mut s, AGENT, &id);
    s.write(AGENT, false, |c| c.unclaim(&id, false, Some("stuck")))
        .unwrap();
    assert_eq!(state(&s, &id), State::Open);
    claim(&mut s, AGENT, &id);
    s.write(HUMAN, false, |c| {
        c.claim(
            &id,
            ClaimOpts {
                steal: true,
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert_eq!(state(&s, &id), State::Claimed);
    let claims = s.read(None, |c| c.claims(&id)).unwrap();
    assert_eq!(claims[1].release_reason.as_deref(), Some("stolen"));
    assert_eq!(claims[2].actor, HUMAN);
    // An open issue can be cancelled without a claim.
    let other = create(&mut s, "Never mind");
    s.write(HUMAN, false, |c| c.close(&other, "abandoned", None, false))
        .unwrap();
    assert_eq!(ids(&s, "reason=abandoned"), vec![other]);
}

#[test]
fn an_expired_lease_returns_the_issue_to_open() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    claim(&mut s, AGENT, &id);
    s.opts.now = Some(ts("2026-11-20T10:20:00Z"));
    assert_eq!(ids(&s, "stale-lease"), Vec::<String>::new());
    s.opts.now = Some(ts("2026-11-20T10:31:00Z"));
    assert_eq!(ids(&s, "stale-lease"), vec![id.clone()]);
    // Any write sweeps.
    create(&mut s, "Other");
    assert_eq!(state(&s, &id), State::Open);
    let claims = s.read(None, |c| c.claims(&id)).unwrap();
    assert_eq!(claims[0].release_reason.as_deref(), Some("expired"));
    assert!(ops_of(&s, &id).contains(&"release".to_string()));
}

#[test]
fn waiting_releases_the_claim_and_keeps_the_worktree() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    // Only a claimed issue can start waiting.
    let e = s
        .write(HUMAN, false, |c| {
            c.wait(
                &id,
                WaitOpts {
                    check: Some("true".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidState);
    claim(&mut s, AGENT, &id);
    // Neither a check nor a reason says nothing.
    let e = s
        .write(AGENT, false, |c| c.wait(&id, WaitOpts::default()))
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidField);
    s.write(AGENT, false, |c| {
        c.wait(
            &id,
            WaitOpts {
                check: Some("git merge feature".into()),
                interval: Some(jiff::SignedDuration::from_mins(2)),
                reason: Some("merge into main".into()),
                force: false,
            },
        )
    })
    .unwrap();
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Waiting);
    assert_eq!(i.wait_cwd.as_deref(), Some("/tmp/wt"));
    assert_eq!(i.check_interval, Some(120));
    assert_eq!(i.next_check_at.as_deref(), Some("2026-11-20T10:00:00Z"));
    assert!(s.read(None, |c| c.active_claim(&id)).unwrap().is_none());
    let claims = s.read(None, |c| c.claims(&id)).unwrap();
    assert_eq!(claims[0].release_reason.as_deref(), Some("waiting"));
    let v = s.read(None, |c| c.show(&id, With::default())).unwrap();
    assert_eq!(v["waiting"]["check"], "git merge feature");
    assert_eq!(v["waiting"]["interval"], 120);
    assert_eq!(v["derived"]["due"], true);
    assert_eq!(ids(&s, "due"), vec![id.clone()]);
    // The waiting session may keep notes without reclaiming the issue.
    s.write(AGENT, false, |c| {
        c.note(&id, "review addressed", None, false)
    })
    .unwrap();
    assert!(s.read(None, |c| c.active_claim(&id)).unwrap().is_none());
    // A waiting issue cannot be claimed; a human changes its check in place.
    let e = s
        .write(HUMAN, false, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidState);
    wait(&mut s, HUMAN, &id, Some("true"), None);
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.waiting_check.as_deref(), Some("true"));
    assert_eq!(i.wait_cwd.as_deref(), Some("/tmp/wt"));
    assert_eq!(i.check_interval, Some(300));
}

#[test]
fn waiting_session_permissions_end_when_waiting_ends() {
    for transition in ["close", "reopen", "pass", "fail"] {
        let (_d, mut s) = setup();
        let id = create(&mut s, "Waiting work");
        claim(&mut s, AGENT, &id);
        wait(&mut s, AGENT, &id, Some("check"), Some("review"));
        let follow = || NewIssue {
            followup_of: Some(id.clone()),
            ..new("Follow-up")
        };
        s.write(AGENT, false, |c| c.create(follow())).unwrap();
        assert_eq!(
            s.write(AGENT2, false, |c| c.note(&id, "not mine", None, false))
                .unwrap_err()
                .code(),
            Code::NotPermitted
        );
        assert_eq!(
            s.write(AGENT2, false, |c| c.create(follow()))
                .unwrap_err()
                .code(),
            Code::NotPermitted
        );
        assert_eq!(
            s.write(AGENT, false, |c| c.close(&id, "done", None, false))
                .unwrap_err()
                .code(),
            Code::NotPermitted
        );
        assert_eq!(
            s.write(AGENT, false, |c| c.wait(&id, WaitOpts::default()))
                .unwrap_err()
                .code(),
            Code::NotPermitted
        );
        assert_eq!(
            s.write(AGENT, false, |c| c.edit(
                &id,
                Edit {
                    title: Some("not allowed".into()),
                    ..Default::default()
                }
            ))
            .unwrap_err()
            .code(),
            Code::NotPermitted
        );
        let runs = s.read(None, |c| c.runs()).unwrap();
        assert_eq!(runs[0]["actor"], AGENT);
        assert_eq!(runs[0]["state"], "waiting");
        match transition {
            "close" => s
                .write(HUMAN, false, |c| c.close(&id, "done", None, false))
                .unwrap(),
            "reopen" => s.write(HUMAN, false, |c| c.reopen(&id, None)).unwrap(),
            "pass" => assert_eq!(
                record(&mut s, &id, "check", &run(0, "")),
                CheckResult::Passed
            ),
            "fail" => assert_eq!(
                record(&mut s, &id, "check", &run(1, "")),
                CheckResult::Failed
            ),
            _ => unreachable!(),
        }
        assert!(s.read(None, |c| c.session_claim(&id)).unwrap().is_none());
        assert!(s.read(None, |c| c.runs()).unwrap().is_empty());
        assert_eq!(
            s.write(AGENT, false, |c| c.note(&id, "too late", None, false))
                .unwrap_err()
                .code(),
            Code::NotPermitted
        );
        assert_eq!(
            s.write(AGENT, false, |c| c.create(follow()))
                .unwrap_err()
                .code(),
            Code::NotPermitted
        );
    }
}

#[test]
fn only_the_latest_session_can_act_when_waiting_again() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    claim(&mut s, AGENT, &id);
    wait(&mut s, AGENT, &id, None, Some("first review"));
    s.write(HUMAN, false, |c| c.reopen(&id, None)).unwrap();
    claim(&mut s, AGENT2, &id);
    assert_eq!(
        s.read(None, |c| c.session_claim(&id))
            .unwrap()
            .unwrap()
            .actor,
        AGENT2
    );
    wait(&mut s, AGENT2, &id, None, Some("second review"));
    s.write(AGENT2, false, |c| {
        c.note(&id, "current session", None, false)
    })
    .unwrap();
    assert_eq!(
        s.write(AGENT, false, |c| c.note(&id, "old session", None, false))
            .unwrap_err()
            .code(),
        Code::NotPermitted
    );
    let runs = s.read(None, |c| c.runs()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["actor"], AGENT2);
    assert_eq!(runs[0]["state"], "waiting");
    assert_eq!(
        s.read(None, |c| c.session_claim(&id))
            .unwrap()
            .unwrap()
            .actor,
        AGENT2
    );
}

#[test]
fn a_check_passes_retries_or_fails() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    claim(&mut s, AGENT, &id);
    wait(
        &mut s,
        AGENT,
        &id,
        Some("check-pr 12"),
        Some("PR 12 merged"),
    );
    let before = ops_of(&s, &id);

    // 75: not yet. No event; due again one interval later.
    s.opts.now = Some(ts("2026-11-20T10:01:00Z"));
    assert_eq!(
        record(&mut s, &id, "check-pr 12", &run(75, "still open")),
        CheckResult::Retry
    );
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Waiting);
    assert_eq!(i.last_check_exit, Some(75));
    assert_eq!(i.next_check_at.as_deref(), Some("2026-11-20T10:06:00Z"));
    assert_eq!(i.updated_at, "2026-11-20T10:00:00Z");
    assert_eq!(ops_of(&s, &id), before);
    assert!(s.read(None, |c| c.check_queue(false)).unwrap().is_empty());
    assert_eq!(s.read(None, |c| c.check_queue(true)).unwrap().len(), 1);
    s.opts.now = Some(ts("2026-11-20T10:06:00Z"));
    assert_eq!(s.read(None, |c| c.check_queue(false)).unwrap().len(), 1);

    // A run of a command the issue no longer waits on is dropped.
    assert_eq!(
        record(&mut s, &id, "something else", &run(0, "")),
        CheckResult::Skipped
    );
    assert_eq!(state(&s, &id), State::Waiting);

    // Anything else: open again, with the output in a note.
    assert_eq!(
        record(&mut s, &id, "check-pr 12", &run(3, "boom\n")),
        CheckResult::Failed
    );
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Open);
    assert_eq!(i.waiting_check, None);
    assert_eq!(i.wait_cwd, None);
    let notes = s.read(None, |c| c.notes(&id)).unwrap();
    let body = notes.last().unwrap()["body"].as_str().unwrap();
    assert!(body.contains("exited with 3"), "{body}");
    assert!(body.contains("boom"), "{body}");
    assert!(body.contains("check-pr 12"), "{body}");
    assert!(body.contains("PR 12 merged"), "{body}");
    assert_eq!(ids(&s, "ready"), vec![id.clone()]);

    // 0: closed as done.
    claim(&mut s, AGENT2, &id);
    wait(&mut s, AGENT2, &id, Some("true"), None);
    assert_eq!(
        record(&mut s, &id, "true", &run(0, "")),
        CheckResult::Passed
    );
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Closed);
    assert_eq!(i.close_reason.as_deref(), Some("done"));
    let ops = ops_of(&s, &id);
    assert_eq!(&ops[ops.len() - 2..], ["check", "close"]);
}

#[test]
fn a_timeout_or_a_killed_check_is_a_failure() {
    let (_d, mut s) = setup();
    for r in [
        CheckRun {
            exit: None,
            timed_out: true,
            output: "slow".into(),
        },
        CheckRun {
            exit: Some(75),
            timed_out: true,
            output: String::new(),
        },
        CheckRun::default(),
    ] {
        let id = create(&mut s, "Work");
        claim(&mut s, AGENT, &id);
        wait(&mut s, AGENT, &id, Some("sleep 999"), None);
        assert_eq!(record(&mut s, &id, "sleep 999", &r), CheckResult::Failed);
        assert_eq!(state(&s, &id), State::Open);
    }
}

#[test]
fn a_failed_check_restores_the_waiting_session_with_a_fresh_claim() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    let original = s
        .write(AGENT, false, |c| {
            c.claim(
                &id,
                ClaimOpts {
                    lease: Some(jiff::SignedDuration::from_hours(2)),
                    worktree: Some("/tmp/session-worktree".into()),
                    host: Some("session-host".into()),
                    agent: Some("implementer".into()),
                    terminal: Some("session-terminal".into()),
                    on_behalf_of: Some(HUMAN.into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    wait(&mut s, AGENT, &id, Some("merge"), Some("land the branch"));
    let history = s.read(None, |c| c.claims(&id)).unwrap();
    s.opts.now = Some(ts("2026-11-20T13:00:00Z"));

    let result = s
        .write(HUMAN, false, |c| {
            c.check_record(&id, "merge", &run(1, "merge conflict\n"), Some(original.id))
        })
        .unwrap();
    let CheckResult::Woken { claim } = result else {
        panic!("expected a restored claim, got {result:?}");
    };
    assert_eq!(result.as_str(), "woken");
    assert_ne!(claim, original.id);
    let active = s.read(None, |c| c.active_claim(&id)).unwrap().unwrap();
    assert_eq!(active.id, claim);
    assert_eq!(active.actor, original.actor);
    assert_eq!(active.on_behalf_of, original.on_behalf_of);
    assert_eq!(active.worktree, original.worktree);
    assert_eq!(active.host, original.host);
    assert_eq!(active.agent, original.agent);
    assert_eq!(active.terminal, original.terminal);
    assert_eq!(active.claimed_at, "2026-11-20T13:00:00Z");
    assert_eq!(active.lease_until.as_deref(), Some("2026-11-20T13:30:00Z"));
    assert!(active.released_at.is_none());
    assert!(active.release_reason.is_none());
    let claims = s.read(None, |c| c.claims(&id)).unwrap();
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0], history[0]);
    let issue = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(issue.state, State::Claimed);
    assert!(issue.waiting_reason.is_none());
    assert!(issue.waiting_check.is_none());
    assert!(issue.check_interval.is_none());
    assert!(issue.next_check_at.is_none());
    assert!(issue.last_check_at.is_none());
    assert!(issue.last_check_exit.is_none());
    assert!(issue.wait_cwd.is_none());

    let events = s.read(None, |c| c.events(&id)).unwrap();
    let recent = &events[events.len() - 3..];
    assert_eq!(recent[0].op, "note");
    let body = recent[0].payload["body"].as_str().unwrap();
    assert!(body.contains("waiting session has been resumed"), "{body}");
    assert!(!body.contains("open again"), "{body}");
    assert!(body.contains("merge conflict"), "{body}");
    assert!(body.contains("land the branch"), "{body}");
    assert!(body.contains("/tmp/session-worktree"), "{body}");
    assert_eq!(recent[1].op, "check");
    assert_eq!(recent[1].payload["result"], "failed");
    assert_eq!(recent[1].payload["exit"], 1);
    assert_eq!(recent[2].op, "claim");
    assert_eq!(recent[2].actor, HUMAN);
    assert_eq!(recent[2].payload["claimant"], AGENT);
    assert_eq!(recent[2].payload["claim"], claim);
    assert_eq!(recent[2].payload["resumed_from"], original.id);
    assert_eq!(recent[2].payload["from"], "waiting");
    assert_eq!(recent[2].payload["by_check"], true);
    assert!(!events.iter().any(|e| e.op == "reopen"));
    // The original actor can immediately work on the issue again.
    s.write(AGENT, false, |c| c.note(&id, "resolving", None, false))
        .unwrap();
}

#[test]
fn stale_or_foreign_resume_candidates_reopen_instead() {
    let (_d, mut s) = setup();
    let foreign = create(&mut s, "Other");
    claim(&mut s, AGENT, &foreign);
    let foreign_claim = s.read(None, |c| c.active_claim(&foreign)).unwrap().unwrap();
    wait(&mut s, AGENT, &foreign, Some("merge"), None);

    for candidate in [None, Some(foreign_claim.id), Some(i64::MAX)] {
        let id = create(&mut s, "Work");
        claim(&mut s, AGENT, &id);
        let stale = s.read(None, |c| c.active_claim(&id)).unwrap().unwrap();
        wait(&mut s, AGENT, &id, Some("merge"), None);
        s.write(HUMAN, false, |c| c.reopen(&id, None)).unwrap();
        claim(&mut s, AGENT2, &id);
        wait(&mut s, AGENT2, &id, Some("merge"), None);
        let history = s.read(None, |c| c.claims(&id)).unwrap();
        let candidate = candidate.or(Some(stale.id));
        assert_eq!(
            s.write(HUMAN, false, |c| {
                c.check_record(&id, "merge", &run(1, "failed"), candidate)
            })
            .unwrap(),
            CheckResult::Failed
        );
        assert_eq!(state(&s, &id), State::Open);
        assert!(s.read(None, |c| c.active_claim(&id)).unwrap().is_none());
        assert_eq!(s.read(None, |c| c.claims(&id)).unwrap(), history);
        let notes = s.read(None, |c| c.notes(&id)).unwrap();
        let body = notes.last().unwrap()["body"].as_str().unwrap();
        assert!(body.contains("open again"), "{body}");
        assert!(!body.contains("has been resumed"), "{body}");
    }
}

#[test]
fn passing_retrying_and_stale_checks_ignore_resume_candidates() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Work");
    claim(&mut s, AGENT, &id);
    let original = s.read(None, |c| c.active_claim(&id)).unwrap().unwrap();
    wait(&mut s, AGENT, &id, Some("merge"), None);
    let events = s.read(None, |c| c.events(&id)).unwrap();
    for (check, exit, expected) in [
        ("other command", 1, CheckResult::Skipped),
        ("merge", 75, CheckResult::Retry),
        ("merge", 0, CheckResult::Passed),
    ] {
        assert_eq!(
            s.write(HUMAN, false, |c| {
                c.check_record(&id, check, &run(exit, ""), Some(original.id))
            })
            .unwrap(),
            expected
        );
        assert!(s.read(None, |c| c.active_claim(&id)).unwrap().is_none());
        assert_eq!(s.read(None, |c| c.claims(&id)).unwrap().len(), 1);
        if expected != CheckResult::Passed {
            assert_eq!(
                serde_json::to_value(s.read(None, |c| c.events(&id)).unwrap()).unwrap(),
                serde_json::to_value(&events).unwrap()
            );
        }
    }
    assert_eq!(state(&s, &id), State::Closed);
}

#[test]
fn timed_out_and_killed_checks_can_restore_the_waiting_session() {
    let (_d, mut s) = setup();
    for (exit, timed_out) in [
        (None, true),
        (Some(75), true),
        (Some(0), true),
        (None, false),
    ] {
        let id = create(&mut s, "Work");
        claim(&mut s, AGENT, &id);
        let original = s.read(None, |c| c.active_claim(&id)).unwrap().unwrap();
        wait(&mut s, AGENT, &id, Some("slow"), None);
        let result = s
            .write(HUMAN, false, |c| {
                c.check_record(
                    &id,
                    "slow",
                    &CheckRun {
                        exit,
                        timed_out,
                        output: "partial output".into(),
                    },
                    Some(original.id),
                )
            })
            .unwrap();
        assert!(matches!(result, CheckResult::Woken { .. }));
        assert_eq!(state(&s, &id), State::Claimed);
    }
}

#[test]
fn failed_wake_delivery_only_releases_the_exact_still_active_claim() {
    let (_d, mut s) = setup();
    for transition in ["active", "open", "closed", "waiting", "replacement"] {
        let id = create(&mut s, "Work");
        claim(&mut s, AGENT, &id);
        let original = s.read(None, |c| c.active_claim(&id)).unwrap().unwrap();
        wait(&mut s, AGENT, &id, Some("merge"), None);
        let result = s
            .write(HUMAN, false, |c| {
                c.check_record(&id, "merge", &run(1, "conflict"), Some(original.id))
            })
            .unwrap();
        let CheckResult::Woken { claim } = result else {
            panic!("expected a restored claim, got {result:?}");
        };
        // Even while claimed, a mismatched id must be a no-op.
        assert!(
            !s.write(HUMAN, false, |c| c.check_wake_failed(&id, original.id))
                .unwrap()
        );
        match transition {
            "open" => s
                .write(AGENT, false, |c| c.unclaim(&id, false, None))
                .unwrap(),
            "closed" => s
                .write(AGENT, false, |c| c.close(&id, "done", None, false))
                .unwrap(),
            "waiting" => wait(&mut s, AGENT, &id, Some("retry"), None),
            "replacement" => {
                s.write(AGENT2, false, |c| {
                    c.claim(
                        &id,
                        ClaimOpts {
                            steal: true,
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
            }
            _ => {}
        }
        let issue = s.read(None, |c| c.issue(&id)).unwrap();
        let claims = s.read(None, |c| c.claims(&id)).unwrap();
        let events = s.read(None, |c| c.events(&id)).unwrap();
        let changed = s
            .write(HUMAN, false, |c| c.check_wake_failed(&id, claim))
            .unwrap();
        assert_eq!(changed, transition == "active");
        if changed {
            assert_eq!(state(&s, &id), State::Open);
            assert!(s.read(None, |c| c.active_claim(&id)).unwrap().is_none());
            let history = s.read(None, |c| c.claims(&id)).unwrap();
            assert_eq!(history[1].release_reason.as_deref(), Some("unclaimed"));
            let events = s.read(None, |c| c.events(&id)).unwrap();
            let recent = &events[events.len() - 2..];
            assert_eq!(recent[0].op, "note");
            assert!(
                recent[0].payload["body"]
                    .as_str()
                    .unwrap()
                    .contains("could not be delivered")
            );
            assert_eq!(recent[1].op, "release");
            assert_eq!(recent[1].payload["claim"], claim);
            assert_eq!(recent[1].payload["check_wake_failed"], true);
            assert!(
                !s.write(HUMAN, false, |c| c.check_wake_failed(&id, claim))
                    .unwrap()
            );
        } else {
            assert_eq!(s.read(None, |c| c.issue(&id)).unwrap(), issue);
            assert_eq!(s.read(None, |c| c.claims(&id)).unwrap(), claims);
            assert_eq!(
                serde_json::to_value(s.read(None, |c| c.events(&id)).unwrap()).unwrap(),
                serde_json::to_value(&events).unwrap()
            );
        }
    }
}

#[test]
fn waiting_on_a_human_is_never_due() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Decide");
    claim(&mut s, AGENT, &id);
    wait(&mut s, AGENT, &id, None, Some("which database?"));
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Waiting);
    assert_eq!(i.next_check_at, None);
    assert_eq!(i.check_interval, None);
    assert!(s.read(None, |c| c.check_queue(true)).unwrap().is_empty());
    assert!(ids(&s, "due").is_empty());
    assert_eq!(ids(&s, "state=waiting"), vec![id.clone()]);
    // The human answers and reopens, or closes.
    s.write(HUMAN, false, |c| c.reopen(&id, Some("SQLite")))
        .unwrap();
    assert_eq!(state(&s, &id), State::Open);
    claim(&mut s, AGENT, &id);
    wait(&mut s, AGENT, &id, None, Some("approval"));
    s.write(HUMAN, false, |c| c.close(&id, "done", None, false))
        .unwrap();
    let i = s.read(None, |c| c.issue(&id)).unwrap();
    assert_eq!(i.state, State::Closed);
    assert_eq!(i.waiting_reason, None);
}

#[test]
fn the_check_queue_runs_higher_priority_first() {
    let (_d, mut s) = setup();
    let mut made = vec![];
    for p in ["P2", "P0", "P1"] {
        let id = s
            .write(HUMAN, false, |c| {
                c.create(NewIssue {
                    priority: Some(p.into()),
                    ..new(p)
                })
            })
            .unwrap();
        claim(&mut s, HUMAN, &id);
        wait(&mut s, HUMAN, &id, Some("true"), None);
        made.push(id);
    }
    let q: Vec<String> = s
        .read(None, |c| c.check_queue(false))
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(q, vec![made[1].clone(), made[2].clone(), made[0].clone()]);
}

#[test]
fn the_check_lock_has_one_holder() {
    let (_d, mut s) = setup();
    assert!(s.read(None, |c| c.check_lock()).unwrap().is_none());
    s.write(HUMAN, false, |c| c.check_lock_take(41, Some("host")))
        .unwrap();
    let (pid, host, _, pgid) = s.read(None, |c| c.check_lock()).unwrap().unwrap();
    assert_eq!((pid, host.as_deref(), pgid), (41, Some("host"), None));
    s.write(HUMAN, false, |c| c.check_lock_group(41, Some(51)))
        .unwrap();
    // Another pid cannot change or release the holder's subprocess group.
    s.write(HUMAN, false, |c| c.check_lock_group(42, Some(52)))
        .unwrap();
    s.write(HUMAN, false, |c| c.check_lock_release(42)).unwrap();
    let lock = s.read(None, |c| c.check_lock()).unwrap().unwrap();
    assert_eq!((lock.0, lock.3), (41, Some(51)));
    s.write(HUMAN, false, |c| c.check_lock_group(41, None))
        .unwrap();
    assert_eq!(s.read(None, |c| c.check_lock()).unwrap().unwrap().3, None);
    s.write(HUMAN, false, |c| c.check_lock_release(41)).unwrap();
    assert!(s.read(None, |c| c.check_lock()).unwrap().is_none());
}

#[test]
fn the_check_lock_group_persists_and_replacement_clears_it() {
    let (d, mut s) = setup();
    s.write(HUMAN, false, |c| {
        c.check_lock_take(41, Some("host"))?;
        c.check_lock_group(41, Some(51))
    })
    .unwrap();
    drop(s);
    let mut s = Store::open(&d.path().join("trk.db"), StoreOpts::default()).unwrap();
    let lock = s.read(None, |c| c.check_lock()).unwrap().unwrap();
    assert_eq!(
        (lock.0, lock.1.as_deref(), lock.3),
        (41, Some("host"), Some(51))
    );
    s.write(HUMAN, false, |c| c.check_lock_take(42, None))
        .unwrap();
    // The old runner cannot clear or release the replacement holder.
    s.write(HUMAN, false, |c| c.check_lock_group(41, None))
        .unwrap();
    s.write(HUMAN, false, |c| c.check_lock_release(41)).unwrap();
    let lock = s.read(None, |c| c.check_lock()).unwrap().unwrap();
    assert_eq!((lock.0, lock.1, lock.3), (42, None, None));
}

#[test]
fn migrating_the_check_lock_preserves_its_holder() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("trk.db");
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(include_str!("../src/schema.sql"))
        .unwrap();
    raw.pragma_update(None, "user_version", 1).unwrap();
    raw.execute(
        "INSERT INTO check_lock (id, pid, host, started_at) VALUES (1, 41, 'host', '2026-11-20T10:00:00Z')",
        [],
    )
    .unwrap();
    drop(raw);

    let mut s = Store::open(&path, StoreOpts::default()).unwrap();
    let lock = s.read(None, |c| c.check_lock()).unwrap().unwrap();
    assert_eq!(
        lock,
        (
            41,
            Some("host".into()),
            "2026-11-20T10:00:00Z".into(),
            Some(0)
        )
    );
    s.write(HUMAN, false, |c| c.check_lock_group(41, Some(51)))
        .unwrap();
    assert_eq!(
        s.read(None, |c| c.check_lock()).unwrap().unwrap().3,
        Some(51)
    );
}

#[test]
fn blocks_edges_are_acyclic() {
    let (_d, mut s) = setup();
    let a = create(&mut s, "A");
    let b = create(&mut s, "B");
    let c3 = create(&mut s, "C");
    assert!(s.write(HUMAN, false, |c| c.dep_add(&a, &b)).unwrap());
    assert!(!s.write(HUMAN, false, |c| c.dep_add(&a, &b)).unwrap());
    s.write(HUMAN, false, |c| c.dep_add(&b, &c3)).unwrap();
    let e = s.write(HUMAN, false, |c| c.dep_add(&c3, &a)).unwrap_err();
    assert_eq!(e.code(), Code::Cycle);
    let e = s.write(HUMAN, false, |c| c.dep_add(&a, &a)).unwrap_err();
    assert_eq!(e.code(), Code::Cycle);
    assert_eq!(ids(&s, &format!("blocks={b}")), vec![a.clone()]);
    assert_eq!(ids(&s, &format!("blocked-by={b}")), vec![c3.clone()]);
    let v = s.read(None, |c| c.show(&b, With::parse("deps"))).unwrap();
    assert_eq!(v["deps"]["blocked_by"][0]["id"], a.as_str());
    assert_eq!(v["deps"]["blocks"][0]["id"], c3.as_str());
    s.write(HUMAN, false, |c| c.dep_rm(&a, &b)).unwrap();
    let e = s.write(HUMAN, false, |c| c.dep_rm(&a, &b)).unwrap_err();
    assert_eq!(e.code(), Code::NotFound);
    assert!(ids(&s, "ready").contains(&b));
}

#[test]
fn agents_file_follow_ups_only_from_issues_they_claim() {
    let (_d, mut s) = setup();
    let parent = create(&mut s, "Parent");
    let e = s
        .write(AGENT, false, |c| c.create(new("Stray")))
        .unwrap_err();
    assert_eq!(e.code(), Code::NotPermitted);
    let follow = |blocks: bool| NewIssue {
        followup_of: Some(parent.clone()),
        blocks: if blocks { vec![parent.clone()] } else { vec![] },
        ..new("Follow-up")
    };
    let e = s
        .write(AGENT, false, |c| c.create(follow(false)))
        .unwrap_err();
    assert_eq!(e.code(), Code::NotPermitted);
    claim(&mut s, AGENT, &parent);
    let f = s.write(AGENT, false, |c| c.create(follow(false))).unwrap();
    assert_eq!(state(&s, &f), State::Open);
    let ev = s.read(None, |c| c.events(&f)).unwrap();
    assert_eq!(ev[0].payload["followup_of"], parent.as_str());
    let g = s.write(AGENT, false, |c| c.create(follow(true))).unwrap();
    assert_eq!(
        s.read(None, |c| c.blocked_by(&parent)).unwrap(),
        vec![g.clone()]
    );
}

#[test]
fn notes_search_and_inbox() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "Leases for claims");
    let seq = s
        .write(HUMAN, false, |c| {
            c.note(&id, "consider a heartbeat", None, true)
        })
        .unwrap();
    s.write(HUMAN, false, |c| {
        c.note(&id, "typo fixed", Some(seq), false)
    })
    .unwrap();
    assert_eq!(ids(&s, "text~heartbeat"), vec![id.clone()]);
    assert_eq!(ids(&s, "text~leases"), vec![id.clone()]);
    assert!(ids(&s, "text~nothing").is_empty());
    claim(&mut s, AGENT, &id);
    let inbox = s.write(AGENT, false, |c| c.take_inbox(&id)).unwrap();
    assert_eq!(inbox.len(), 1);
    assert!(
        s.write(AGENT, false, |c| c.take_inbox(&id))
            .unwrap()
            .is_empty()
    );
    // The event log is append-only.
    assert!(s.conn().execute("DELETE FROM event", []).is_err());
    assert!(s.conn().execute("DELETE FROM issue", []).is_err());
}

#[test]
fn counts_and_dry_run() {
    let (_d, mut s) = setup();
    let a = create(&mut s, "A");
    create(&mut s, "B");
    s.write(HUMAN, true, |c| c.close(&a, "done", None, false))
        .unwrap();
    assert_eq!(state(&s, &a), State::Open);
    s.write(HUMAN, false, |c| c.close(&a, "done", None, false))
        .unwrap();
    let n = s.read(None, |c| c.counts()).unwrap();
    assert_eq!(n["open"], 1);
    assert_eq!(n["closed"], 1);
}

#[test]
fn issue_types_are_required_authored_fields() {
    let (_dir, mut s) = setup();
    for invalid in ["", " \n "] {
        let error = s
            .write(HUMAN, false, |c| {
                c.create(NewIssue {
                    issue_type: Some(invalid.into()),
                    ..new("Invalid type")
                })
            })
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidField);
    }
    let id = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                issue_type: Some(" review ".into()),
                ..new("Typed issue")
            })
        })
        .unwrap();
    assert_eq!(s.read(None, |c| c.issue(&id)).unwrap().issue_type, "review");
    let changes = s
        .write(HUMAN, false, |c| {
            c.edit(
                &id,
                Edit {
                    issue_type: Some("investigation".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(changes["type"]["from"], "review");
    assert_eq!(changes["type"]["to"], "investigation");
    for invalid in ["", " \n "] {
        let error = s
            .write(HUMAN, false, |c| {
                c.edit(
                    &id,
                    Edit {
                        issue_type: Some(invalid.into()),
                        ..Default::default()
                    },
                )
            })
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidField);
    }
    assert_eq!(
        s.read(None, |c| c.issue(&id)).unwrap().issue_type,
        "investigation"
    );
    let shown = s.read(None, |c| c.show(&id, With::default())).unwrap();
    assert_eq!(shown["type"], "investigation");
    assert!(shown.get("agent").is_none());
    let serialized = serde_json::to_value(s.read(None, |c| c.issue(&id)).unwrap()).unwrap();
    assert_eq!(serialized["type"], "investigation");
    assert!(serialized.get("agent").is_none());
    assert!(
        s.conn()
            .execute("UPDATE issue SET type = '' WHERE id = ?1", [&id])
            .is_err()
    );
}

#[test]
fn queries_and_sorts_use_type_not_issue_agent() {
    let (_dir, mut s) = setup();
    let default = create(&mut s, "Default");
    let custom = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                issue_type: Some("review".into()),
                ..new("Custom")
            })
        })
        .unwrap();
    assert_eq!(ids(&s, "type=code-change"), [default.clone()]);
    assert_eq!(ids(&s, "type=review"), [custom.clone()]);
    let sorted = s.read(None, |c| c.list("", Some("-type"), None)).unwrap();
    assert_eq!(sorted[0]["id"], custom);
    assert_eq!(sorted[1]["id"], default);
    assert_eq!(
        s.read(None, |c| c.list("agent=implementer", None, None))
            .unwrap_err()
            .code(),
        Code::BadQuery
    );
    assert!(trk_core::query::order_by(Some("agent")).is_err());
}
