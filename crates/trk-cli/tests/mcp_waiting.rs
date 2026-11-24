//! Regression coverage for MCP sessions that remain connected after waiting.

use serde_json::{Value, json};
use trk_core::config::ConfigDir;
use trk_core::ops::{ClaimOpts, NewIssue, WaitOpts};
use trk_core::{State, Store, StoreOpts};
use trk_mcp::{McpArgs, TrkServer};

const HUMAN: &str = "human:alice@example.com";
const AGENT: &str = "agent:omp:waiting";
const OTHER: &str = "agent:omp:replacement";

struct Env {
    dir: tempfile::TempDir,
    store: Store,
    opts: StoreOpts,
    issue: String,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in trk_core::seed::preset("default").unwrap() {
            let path = dir.path().join("seed").join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let opts = StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            prefix: "orch".into(),
            ..Default::default()
        };
        let mut store = Store::install(&dir.path().join("trk.db"), opts.clone()).unwrap();
        let issue = store
            .write(HUMAN, false, |c| {
                c.create(NewIssue {
                    title: "Waiting MCP session".into(),
                    description: "Keep the current session available for messages.".into(),
                    ..Default::default()
                })
            })
            .unwrap();
        store
            .write(AGENT, false, |c| c.claim(&issue, ClaimOpts::default()))
            .unwrap();
        Self {
            dir,
            store,
            opts,
            issue,
        }
    }

    fn server(&self, actor: &str) -> TrkServer {
        TrkServer::new(McpArgs {
            db: self.dir.path().join("trk.db"),
            config: ConfigDir::new(self.dir.path().join("seed")),
            project_root: self.dir.path().to_path_buf(),
            user: trk_core::config::UserConfig::default(),
            opts: self.opts.clone(),
            issue: self.issue.clone(),
            actor: actor.into(),
            agent: "implementer".into(),
            base: "main".into(),
        })
        .unwrap()
    }

    fn send(&mut self, body: &str) {
        self.store
            .write(HUMAN, false, |c| c.note(&self.issue, body, None, true))
            .unwrap();
    }

    fn state(&self) -> State {
        self.store
            .read(None, |c| Ok(c.issue(&self.issue)?.state))
            .unwrap()
    }
}

fn success(server: &TrkServer, tool: &str, args: Value) -> Value {
    let (ok, result) = server.call_sync(tool, args);
    assert!(ok, "{tool}: {result:#}");
    result
}

fn start_waiting(server: &TrkServer) {
    success(server, "wait", json!({"reason": "Awaiting review"}));
}

fn no_delivery(server: &TrkServer) {
    let result = success(server, "get_task", json!({}));
    assert!(result.get("messages_for_you").is_none(), "{result:#}");
}

fn delivered(result: &Value, body: &str) {
    let messages = result["messages_for_you"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "{result:#}");
    assert_eq!(messages[0]["body"], body);
    assert_eq!(messages[0]["from"], HUMAN);
}

#[test]
fn waiting_mcp_can_note_and_report_followups_but_cannot_end_again() {
    let e = Env::new();
    let server = e.server(AGENT);
    start_waiting(&server);

    let note = success(
        &server,
        "add_note",
        json!({"body": "Review is still pending"}),
    );
    assert_eq!(note["task"]["state"], "waiting");
    let followup = success(
        &server,
        "report_followup",
        json!({
            "title": "Additional coverage",
            "description": "Record the next task while awaiting review.",
            "blocks_this": true
        }),
    );
    assert_eq!(followup["task"]["state"], "waiting");
    let created = followup["result"]["created"].as_str().unwrap();
    let task = success(&server, "get_task", json!({}));
    assert!(
        task["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note["body"] == "Review is still pending")
    );
    assert!(
        task["deps"]["blocked_by"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["id"] == created)
    );
    assert_eq!(
        e.store.read(None, |c| Ok(c.issue(created)?.title)).unwrap(),
        "Additional coverage"
    );

    for (tool, args, code) in [
        (
            "wait",
            json!({"reason": "Replace original waiting reason"}),
            "not_permitted",
        ),
        (
            "close",
            json!({"reason": "done", "note": "Do not close"}),
            "not_permitted",
        ),
        ("hand_back", json!({"note": "Do not reopen"}), "invalid_state"),
    ] {
        let (ok, error) = server.call_sync(tool, args);
        assert!(!ok, "{tool}: {error:#}");
        assert_eq!(error["error"]["code"], code, "{error:#}");
        assert_eq!(e.state(), State::Waiting);
    }
    let task = success(&server, "get_task", json!({}));
    assert_eq!(task["waiting"]["reason"], "Awaiting review");
    assert!(
        !task["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note["body"] == "Do not close" || note["body"] == "Do not reopen")
    );
    // Post-wait writes must not reactivate or renew the released claim.
    let claims = e.store.read(None, |c| c.claims(&e.issue)).unwrap();
    assert_eq!(claims.len(), 1);
    assert!(claims[0].released_at.is_some());
    assert_eq!(claims[0].release_reason.as_deref(), Some("waiting"));
    assert!(
        e.store
            .read(None, |c| c.active_claim(&e.issue))
            .unwrap()
            .is_none()
    );
}

#[test]
fn waiting_mcp_inbox_is_delivered_once_only_to_its_actor() {
    let mut e = Env::new();
    let server = e.server(AGENT);
    let unrelated = e.server(OTHER);
    start_waiting(&server);
    e.send("Review needs a small revision");

    no_delivery(&unrelated);
    let result = success(&server, "add_note", json!({"body": "Checking review"}));
    delivered(&result, "Review needs a small revision");
    no_delivery(&server);
}

#[test]
fn waiting_mcp_inbox_is_delivered_on_the_next_failed_tool_result() {
    let mut e = Env::new();
    let server = e.server(AGENT);
    start_waiting(&server);
    e.send("Please read this before ending the session");

    let (ok, result) = server.call_sync("close", json!({"reason": "done", "note": "No"}));
    assert!(!ok, "{result:#}");
    assert_eq!(result["error"]["code"], "not_permitted");
    delivered(&result, "Please read this before ending the session");
    no_delivery(&server);
    assert_eq!(e.state(), State::Waiting);
}

#[test]
fn stale_mcp_cannot_consume_after_close_reopen_or_replacement() {
    let mut e = Env::new();
    let old = e.server(AGENT);
    start_waiting(&old);
    e.send("Pending for the next valid session");

    e.store
        .write(HUMAN, false, |c| c.close(&e.issue, "done", None, false))
        .unwrap();
    assert_eq!(e.state(), State::Closed);
    no_delivery(&old);
    e.store
        .write(HUMAN, false, |c| c.reopen(&e.issue, None))
        .unwrap();
    assert_eq!(e.state(), State::Open);
    no_delivery(&old);

    e.store
        .write(OTHER, false, |c| c.claim(&e.issue, ClaimOpts::default()))
        .unwrap();
    no_delivery(&old);
    let replacement = e.server(OTHER);
    let result = success(&replacement, "get_task", json!({}));
    delivered(&result, "Pending for the next valid session");
    no_delivery(&replacement);

    // Once the replacement waits, it remains the owner, not the older waiter.
    start_waiting(&replacement);
    e.send("Only the replacement waiter should receive this");
    no_delivery(&old);
    let result = success(&replacement, "get_task", json!({}));
    delivered(&result, "Only the replacement waiter should receive this");
    no_delivery(&replacement);
}

#[test]
fn human_waiting_does_not_resurrect_an_older_agent_mcp_session() {
    let mut e = Env::new();
    let old = e.server(AGENT);
    start_waiting(&old);
    e.send("Keep pending until an agent claims again");
    e.store
        .write(HUMAN, false, |c| {
            c.reopen(&e.issue, None)?;
            c.claim(&e.issue, ClaimOpts::default())?;
            c.wait(
                &e.issue,
                WaitOpts {
                    reason: Some("Human review".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    no_delivery(&old);
    e.store
        .write(HUMAN, false, |c| c.reopen(&e.issue, None))
        .unwrap();
    e.store
        .write(OTHER, false, |c| c.claim(&e.issue, ClaimOpts::default()))
        .unwrap();
    let result = success(&e.server(OTHER), "get_task", json!({}));
    delivered(&result, "Keep pending until an agent claims again");
}
