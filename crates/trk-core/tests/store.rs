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
        ..Default::default()
    };
    Store::open(&dir.path().join("trk.db"), opts).unwrap()
}

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store_at(&dir, "2026-11-20T10:00:00Z");
    s.write(HUMAN, false, |c| {
        c.project_add("orch", "Orchestra")?;
        c.repo_add("orch", Some("git@github.com:acme/orch.git"), None)?;
        Ok(())
    })
    .unwrap();
    (dir, s)
}

fn new(title: &str) -> NewIssue {
    NewIssue {
        project: "orch".into(),
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
    s.write(HUMAN, false, |c| c.check_record(id, check, r))
        .unwrap()
}

fn ids(s: &Store, q: &str) -> Vec<String> {
    s.read(Some(HUMAN), |c| c.list(q, None, None, None))
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
    let e = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                project: "nope".into(),
                ..new("x")
            })
        })
        .unwrap_err();
    assert_eq!(e.code(), Code::NotFound);
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
fn the_working_directory_repo_becomes_the_issue_repo() {
    let (dir, s) = setup();
    drop(s);
    let mut s = Store::open(
        &dir.path().join("trk.db"),
        StoreOpts {
            current_repo: Some("github.com/acme/orch".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let id = create(&mut s, "In a repo");
    let v = s.read(None, |c| c.show(&id, With::default())).unwrap();
    assert_eq!(v["repo"]["remote_url"], "github.com/acme/orch");
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
    // The agent has let go: it can no longer write.
    let e = s
        .write(AGENT, false, |c| c.note(&id, "x", None, false))
        .unwrap_err();
    assert_eq!(e.code(), Code::NotPermitted);
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
    let (pid, host, _) = s.read(None, |c| c.check_lock()).unwrap().unwrap();
    assert_eq!((pid, host.as_deref()), (41, Some("host")));
    // Releasing with another pid leaves it alone.
    s.write(HUMAN, false, |c| c.check_lock_release(42)).unwrap();
    assert!(s.read(None, |c| c.check_lock()).unwrap().is_some());
    s.write(HUMAN, false, |c| c.check_lock_release(41)).unwrap();
    assert!(s.read(None, |c| c.check_lock()).unwrap().is_none());
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
fn an_old_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE issue_type (name TEXT PRIMARY KEY); PRAGMA user_version = 2;",
        )
        .unwrap();
    }
    let e = Store::open(&path, StoreOpts::default()).err().unwrap();
    assert_eq!(e.code(), Code::OldDatabase);
    assert!(e.hint().unwrap().contains("import-v1.sh"));
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
    let sorted = s
        .read(None, |c| c.list("", None, Some("-type"), None))
        .unwrap();
    assert_eq!(sorted[0]["id"], custom);
    assert_eq!(sorted[1]["id"], default);
    assert_eq!(
        s.read(None, |c| c.list("agent=implementer", None, None, None))
            .unwrap_err()
            .code(),
        Code::BadQuery
    );
    assert!(trk_core::query::order_by(Some("agent"), false).is_err());
}

fn old_agent_database(path: &std::path::Path, agents: &[Option<&str>]) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(include_str!("../src/schema.sql"))
        .unwrap();
    conn.execute_batch(
        "PRAGMA user_version = 1;
         INSERT INTO project VALUES ('orch', 'Orchestra', '2026-11-20T10:00:00Z');
         INSERT INTO actor (id, kind, first_seen)
         VALUES ('human:alice@example.com', 'human', '2026-11-20T10:00:00Z');",
    )
    .unwrap();
    for (index, agent) in agents.iter().enumerate() {
        conn.execute(
            "INSERT INTO issue
             (id, project_id, title, agent, created_by, created_at, updated_at)
             VALUES (?1, 'orch', 'Old issue', ?2, ?3, '2026-11-20T10:00:00Z', '2026-11-20T10:00:00Z')",
            rusqlite::params![format!("orch-{index}"), agent, HUMAN],
        )
        .unwrap();
    }
}

fn migrated_type(agent: &str) -> String {
    let hex: String = agent.bytes().map(|byte| format!("{byte:02x}")).collect();
    format!("migrated-agent-{hex}")
}

fn assert_new_issue_schema(conn: &rusqlite::Connection) {
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(issue)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(columns.iter().any(|column| column == "type"));
    assert!(!columns.iter().any(|column| column == "agent"));
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn migrating_agents_preserves_explicit_and_implicit_routing_and_claims() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let config = dir.path().join("config");
    old_agent_database(&path, &[None, Some("custom reviewer"), Some("réviseur")]);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO harness VALUES ('omp');
             INSERT INTO actor (id, kind, harness, first_seen)
             VALUES ('agent:omp:s1', 'agent', 'omp', '2026-11-20T10:00:00Z');
             UPDATE issue SET state = 'claimed' WHERE id = 'orch-0';
             INSERT INTO claim (issue_id, actor, claimed_at, agent)
             VALUES ('orch-0', 'agent:omp:s1', '2026-11-20T10:00:00Z', 'actual-launched-agent');
             INSERT INTO dep (src, dst, created_by, created_at)
             VALUES ('orch-0', 'orch-1', 'human:alice@example.com', '2026-11-20T10:00:00Z');
             INSERT INTO event (issue_id, actor, at, op, payload)
             VALUES ('orch-0', 'human:alice@example.com', '2026-11-20T10:00:00Z', 'note', '{\"body\":\"migration history\"}');",
        )
        .unwrap();
    }
    let s = Store::open(
        &path,
        StoreOpts {
            migration_config: Some(config.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_new_issue_schema(s.conn());
    for (index, agent) in ["implementer", "custom reviewer", "réviseur"]
        .iter()
        .enumerate()
    {
        let name = migrated_type(agent);
        let issue = s.read(None, |c| c.issue(&format!("orch-{index}"))).unwrap();
        assert_eq!(issue.issue_type, name);
        let source =
            std::fs::read_to_string(config.join("types").join(format!("{name}.toml"))).unwrap();
        let definition: toml::Value = toml::from_str(&source).unwrap();
        assert_eq!(definition["name"].as_str(), Some(name.as_str()));
        assert_eq!(definition["agent"].as_str(), Some(*agent));
        assert!(
            definition["description"]
                .as_str()
                .unwrap()
                .contains("Migrated")
        );
        assert!(definition["description"].as_str().unwrap().contains(agent));
    }
    assert_eq!(
        s.read(None, |c| c.active_claim("orch-0"))
            .unwrap()
            .unwrap()
            .agent
            .as_deref(),
        Some("actual-launched-agent")
    );
    assert_eq!(ids(&s, "blocked"), ["orch-1"]);
    assert_eq!(ids(&s, "text~history"), ["orch-0"]);
    assert_eq!(s.read(None, |c| c.events("orch-0")).unwrap().len(), 1);
}

#[test]
fn migration_uses_old_config_default_without_affecting_new_issues() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    old_agent_database(&path, &[None, Some("implementer")]);
    let config = dir.path().join("config");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[run]\ndefault_agent = 'old-default'\n",
    )
    .unwrap();
    let mut s = Store::open(
        &path,
        StoreOpts {
            migration_config: Some(config),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        s.read(None, |c| c.issue("orch-0")).unwrap().issue_type,
        migrated_type("old-default")
    );
    assert_eq!(
        s.read(None, |c| c.issue("orch-1")).unwrap().issue_type,
        migrated_type("implementer")
    );
    let id = create(&mut s, "After migration");
    assert_eq!(
        s.read(None, |c| c.issue(&id)).unwrap().issue_type,
        "code-change"
    );
}

#[test]
fn migration_requires_config_and_refuses_conflicting_types_without_losing_agents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    old_agent_database(&path, &[None, Some("reviewer")]);
    let error = Store::open(&path, StoreOpts::default()).err().unwrap();
    assert!(error.to_string().contains("migration_config"));
    let config = dir.path().join("config");
    let types = config.join("types");
    std::fs::create_dir_all(&types).unwrap();
    let conflict = types.join(format!("{}.toml", migrated_type("reviewer")));
    std::fs::write(&conflict, "name = 'unrelated'\nagent = 'wrong'\n").unwrap();
    let opts = StoreOpts {
        migration_config: Some(config),
        ..Default::default()
    };
    let error = Store::open(&path, opts.clone()).err().unwrap();
    assert!(error.to_string().contains("conflicts"));
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT agent FROM issue WHERE id = 'orch-1'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "reviewer"
    );
    assert!(conn.prepare("SELECT type FROM issue").is_err());
    drop(conn);
    std::fs::remove_file(conflict).unwrap();
    let s = Store::open(&path, opts.clone()).unwrap();
    assert_new_issue_schema(s.conn());
    drop(s);
    // A second old database reuses the matching files produced by the first.
    let second = dir.path().join("second.db");
    old_agent_database(&second, &[None, Some("reviewer")]);
    let s = Store::open(&second, opts).unwrap();
    assert_new_issue_schema(s.conn());
    assert_eq!(
        s.read(None, |c| c.issue("orch-1")).unwrap().issue_type,
        migrated_type("reviewer")
    );
    drop(s);
    // Once migrated, opening needs no migration-only configuration.
    assert_new_issue_schema(Store::open(&second, StoreOpts::default()).unwrap().conn());
}

#[test]
fn empty_databases_need_no_migration_config() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = Store::open(&dir.path().join("fresh.db"), StoreOpts::default()).unwrap();
    assert_new_issue_schema(fresh.conn());
    let old = dir.path().join("empty-old.db");
    old_agent_database(&old, &[]);
    let migrated = Store::open(&old, StoreOpts::default()).unwrap();
    assert_new_issue_schema(migrated.conn());
}
