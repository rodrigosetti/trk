//! trk-mcp: the per-issue MCP server (`trk mcp --issue … --actor … --agent …`).
//!
//! The server serves exactly one issue: no tool takes an issue id. It renews
//! the claim's lease while it runs, so a crashed session releases its claim
//! when the lease expires. The core is synchronous; every tool call runs on
//! `spawn_blocking` with its own connection.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeRequestParams, InitializeResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};
use trk_agent::agent::AgentDef;
use trk_agent::exec::{Session, call, error_json};
use trk_agent::tools::ToolDef;
use trk_core::config::{ConfigDir, UserConfig};
use trk_core::view::With;
use trk_core::{Store, StoreOpts};

pub struct McpArgs {
    pub db: PathBuf,
    pub config: ConfigDir,
    pub project_root: PathBuf,
    pub user: UserConfig,
    pub opts: StoreOpts,
    pub issue: String,
    pub actor: String,
    pub agent: String,
    pub base: String,
}

struct Inner {
    db: PathBuf,
    opts: StoreOpts,
    session: Session,
    agent: AgentDef,
    tools: Vec<ToolDef>,
    instructions: String,
    routing: trk_agent::routing::Routing,
}

#[derive(Clone)]
pub struct TrkServer {
    inner: Arc<Inner>,
}

const TASK_URI: &str = "trk://task";

impl TrkServer {
    /// Load the agent, select its tools, render the instructions.
    pub fn new(mut args: McpArgs) -> anyhow::Result<TrkServer> {
        args.opts.known_types = args
            .config
            .files_in("types", "toml")
            .into_iter()
            .filter(|path| path.is_file())
            .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        let store = Store::open(&args.db, args.opts.clone())?;
        let (issue_json, worktree) = store.read(Some(&args.actor), |c| {
            let id = c.resolve_id(&args.issue)?;
            let issue = c.show(
                &id,
                With {
                    deps: true,
                    ..Default::default()
                },
            )?;
            let worktree = issue["claim"]["worktree"].as_str().map(String::from);
            Ok((issue, worktree))
        })?;
        let config = trk_agent::routing::RoutingConfig::new(&args.config);
        let routing = config.resolve(
            issue_json["type"].as_str().unwrap_or_default(),
            Some(&args.agent),
        )?;
        let resolved = config.load(&routing)?;
        let agent = resolved.agent;
        let mut tools = trk_agent::agent_tools(&agent, &args.user);
        if let Some(tool) = tools.iter_mut().find(|tool| tool.name == "report_followup") {
            let types = config.types()?;
            let descriptions = types
                .iter()
                .map(|(name, routing)| format!("{name}: {}", routing.description))
                .collect::<Vec<_>>()
                .join("; ");
            let field = &mut tool.input_schema["properties"]["type"];
            field["enum"] = json!(args.opts.known_types);
            field["description"] = json!(format!(
                "Work type for the new issue; default {}. Project types: {descriptions}",
                args.user.issues.default_type
            ));
        }
        let input = trk_agent::prompt::PromptInput {
            issue: &issue_json,
            project_root: &args.project_root,
            config: &args.user,
            issue_type: &routing.issue_type,
            type_description: &routing.description,
            agent: &agent,
            tools: &tools,
            worktree: worktree.as_deref(),
            actor: &args.actor,
            harness: args.actor.split(':').nth(1),
            base: &args.base,
            verify: &args.user.check.verify,
        };
        let instructions = trk_agent::prompt::instructions(&resolved.config, &input)?;
        let issue = issue_json["id"].as_str().unwrap_or(&args.issue).to_string();
        Ok(TrkServer {
            inner: Arc::new(Inner {
                db: args.db,
                opts: args.opts,
                session: Session {
                    issue,
                    actor: args.actor,
                },
                agent,
                tools,
                instructions,
                routing,
            }),
        })
    }

    pub fn tools(&self) -> &[ToolDef] {
        &self.inner.tools
    }

    fn open(&self) -> trk_core::Result<Store> {
        Store::open(&self.inner.db, self.inner.opts.clone())
    }

    /// Run one tool call synchronously (also used by tests).
    pub fn call_sync(&self, name: &str, args: Value) -> (bool, Value) {
        let Some(tool) = self.inner.tools.iter().find(|t| t.name == name).cloned() else {
            return (
                false,
                json!({"code": "not_found", "message": format!("unknown tool '{name}'"),
                       "hint": format!("tools: {}", self.inner.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "))}),
            );
        };
        let mut store = match self.open() {
            Ok(s) => s,
            Err(e) => return (false, error_json(&e)),
        };
        let (ok, mut out) = match call(&mut store, &self.inner.session, &tool, &args) {
            Ok(v) => (true, v),
            Err(e) => (false, json!({"ok": false, "error": error_json(&e)})),
        };
        if ok && name == "get_task" {
            out["routing"] = json!(self.inner.routing);
        }
        if self.inner.agent.inbox {
            let msgs = store.write(&self.inner.session.actor, false, |c| {
                if c.session_claim(&self.inner.session.issue)?
                    .is_some_and(|cl| cl.actor == c.actor_str())
                {
                    c.take_inbox(&self.inner.session.issue)
                } else {
                    Ok(vec![])
                }
            });
            if let Ok(msgs) = msgs
                && !msgs.is_empty()
                && let Value::Object(m) = &mut out
            {
                m.insert("messages_for_you".into(), json!(msgs));
            }
        }
        (ok, out)
    }

    fn renew_lease(&self) {
        let Ok(mut store) = self.open() else { return };
        let s = &self.inner.session;
        let _ = store.write(&s.actor, false, |c| {
            if c.active_claim(&s.issue)?
                .is_some_and(|cl| cl.actor == s.actor && cl.lease_until.is_some())
            {
                c.renew(&s.issue, None)?;
            }
            Ok(())
        });
    }

    fn record_started(&self) {
        let host = std::process::Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        self.patch_meta(json!({
            "mcp_pid": std::process::id(),
            "mcp_host": host,
            "mcp_started_at": trk_core::model::fmt_ts(jiff::Timestamp::now()),
            "mcp_exited_at": null,
        }));
    }

    fn patch_meta(&self, patch: Value) {
        let Ok(mut store) = self.open() else { return };
        let actor = self.inner.session.actor.clone();
        let _ = store.write(&actor, false, |c| c.patch_actor_meta(&actor, patch));
    }
}

fn to_tool(t: &ToolDef) -> Tool {
    let schema = match &t.input_schema {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    Tool::new(t.name.clone(), t.description.clone(), Arc::new(schema))
}

impl ServerHandler for TrkServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("trk", env!("CARGO_PKG_VERSION")))
        .with_instructions(self.inner.instructions.clone())
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        context.peer.set_peer_info(request.clone());
        let client = json!({"client": {"name": request.client_info.name, "version": request.client_info.version}});
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.patch_meta(client)).await;
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(
            self.inner.tools.iter().map(to_tool).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.inner
            .tools
            .iter()
            .find(|t| t.name == name)
            .map(to_tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        let args = Value::Object(request.arguments.unwrap_or_default());
        let this = self.clone();
        let (ok, out) = tokio::task::spawn_blocking(move || this.call_sync(&name, args))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let text = serde_json::to_string_pretty(&out).unwrap_or_default();
        let result = if ok {
            CallToolResult::success(vec![ContentBlock::text(text)])
        } else {
            CallToolResult::error(vec![ContentBlock::text(text)])
        };
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(TASK_URI, "task")
                .with_description(format!(
                    "The issue {} this session works on (same as get_task)",
                    self.inner.session.issue
                ))
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != TASK_URI {
            return Err(McpError::resource_not_found(
                format!("unknown resource {}", request.uri),
                None,
            ));
        }
        let this = self.clone();
        let v = tokio::task::spawn_blocking(move || {
            let store = this.open()?;
            store.read(Some(&this.inner.session.actor), |c| {
                c.show(
                    &this.inner.session.issue,
                    With {
                        deps: true,
                        notes: true,
                        ..Default::default()
                    },
                )
            })
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(|e| McpError::internal_error(e.message(), Some(e.to_json())))?;
        let text = serde_json::to_string_pretty(&v).unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, TASK_URI).with_mime_type("application/json"),
        ])
        .into())
    }
}

/// Serve over stdio until the client disconnects.
pub fn serve(args: McpArgs) -> anyhow::Result<()> {
    let lease = args.opts.agent_lease;
    let server = TrkServer::new(args)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let s = server.clone();
        let _ = tokio::task::spawn_blocking(move || s.record_started()).await;

        // Renew the lease well before it runs out.
        let secs = (lease.as_secs() / 3).clamp(5, 300) as u64;
        let renewer = server.clone();
        let renew = tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs));
            loop {
                tick.tick().await;
                let r = renewer.clone();
                let _ = tokio::task::spawn_blocking(move || r.renew_lease()).await;
            }
        });

        let service = server.clone().serve(rmcp::transport::stdio()).await?;
        let res = service.waiting().await;
        renew.abort();
        let s = server.clone();
        let exited = json!({"mcp_exited_at": trk_core::model::fmt_ts(jiff::Timestamp::now())});
        let _ = tokio::task::spawn_blocking(move || s.patch_meta(exited)).await;
        res?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trk_core::State;
    use trk_core::model::Claim;
    use trk_core::ops::{CheckResult, CheckRun, ClaimOpts, NewIssue};

    const ACTOR: &str = "agent:omp:waiting-session";
    const HUMAN: &str = "human:test@example.com";

    fn write_seed(root: &std::path::Path) {
        for (path, content) in trk_core::seed::preset("default").unwrap() {
            let rendered = (path == "config.toml").then(|| {
                std::str::from_utf8(content)
                    .unwrap()
                    .replace("{{EMAIL_LINE}}", "email = \"test@example.com\"")
                    .replace("{{PREFIX}}", "trk")
                    .replace("{{BASE}}", "\"main\"")
            });
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                rendered.as_deref().map(str::as_bytes).unwrap_or(content),
            )
            .unwrap();
        }
    }

    fn active_claim(store: &Store, issue: &str) -> Option<Claim> {
        store.read(None, |c| c.active_claim(issue)).unwrap()
    }

    fn assert_released(server: &TrkServer, state: State) {
        let (ok, out) = server.call_sync("add_note", json!({"body": "released session note"}));
        if state == State::Waiting {
            assert!(ok, "{out}");
            assert_eq!(out["task"]["state"], "waiting");
            assert_eq!(out["task"]["claimed"], false);
        } else {
            assert!(!ok, "{out}");
            assert_eq!(out["error"]["code"], "not_permitted");
        }
        for (tool, args) in [
            ("wait", json!({"reason": "must not wait again"})),
            ("close", json!({"reason": "done", "note": "must not close"})),
            ("hand_back", json!({"note": "must not hand back"})),
        ] {
            let (ok, out) = server.call_sync(tool, args);
            assert!(!ok, "{tool}: {out}");
            let code = if tool == "hand_back" {
                "invalid_state"
            } else {
                "not_permitted"
            };
            assert_eq!(out["error"]["code"], code, "{tool}: {out}");
        }
    }

    #[test]
    fn wait_returns_human_resume_guidance_only_without_a_check() {
        for check in [None, Some(" \n\t"), Some("exit 75")] {
            let dir = tempfile::tempdir().unwrap();
            let seed = dir.path().join("seed");
            write_seed(&seed);
            let db = dir.path().join("trk.db");
            let opts = StoreOpts::default();
            let mut store = Store::install(&db, opts.clone()).unwrap();
            let issue = store
                .write(HUMAN, false, |c| {
                    c.create(NewIssue {
                        title: "Choose a store".into(),
                        ..Default::default()
                    })
                })
                .unwrap();
            store
                .write(ACTOR, false, |c| c.claim(&issue, ClaimOpts::default()))
                .unwrap();
            let server = TrkServer::new(McpArgs {
                db,
                config: ConfigDir::with_user_dir(&seed, None),
                project_root: dir.path().to_path_buf(),
                user: UserConfig::default(),
                opts,
                issue: issue.clone(),
                actor: ACTOR.into(),
                agent: "implementer".into(),
                base: "main".into(),
            })
            .unwrap();
            let mut args = json!({"reason": "SQLite or Dolt?"});
            if let Some(check) = check {
                args["check"] = json!(check);
            }
            let (ok, out) = server.call_sync("wait", args);
            assert!(ok, "{out}");
            assert_eq!(out["waiting"], issue);
            let message = out["message"].as_str().unwrap();
            if check.is_some_and(|s| !s.trim().is_empty()) {
                assert!(!message.contains("trk resume"), "{message}");
                assert!(message.contains("trk runs the check"), "{message}");
            } else {
                assert!(
                    message.ends_with(&format!("trk resume {issue} -m \"<your decision>\"")),
                    "{message}"
                );
                assert!(
                    message.contains("a chat reply alone will not continue the work"),
                    "{message}"
                );
                assert!(
                    message.contains("repeat the command rather than only recording a note"),
                    "{message}"
                );
            }
            assert_released(&server, State::Waiting);
        }
    }

    #[test]
    fn followups_use_the_configured_default_type_unless_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let seed = dir.path().join("seed");
        write_seed(&seed);
        std::fs::write(
            seed.join("types/task.toml"),
            "description = \"Project task\"\nagent = \"implementer\"\n",
        )
        .unwrap();
        let db = dir.path().join("trk.db");
        let mut user = UserConfig::default();
        user.issues.default_type = "task".into();
        let opts = StoreOpts {
            default_type: user.issues.default_type.clone(),
            ..Default::default()
        };
        let mut store = Store::install(&db, opts.clone()).unwrap();
        let issue = store
            .write(HUMAN, false, |c| {
                c.create(NewIssue {
                    title: "Parent issue".into(),
                    issue_type: Some("code-change".into()),
                    ..Default::default()
                })
            })
            .unwrap();
        store
            .write(ACTOR, false, |c| c.claim(&issue, ClaimOpts::default()))
            .unwrap();
        let server = TrkServer::new(McpArgs {
            db,
            config: ConfigDir::with_user_dir(&seed, None),
            project_root: dir.path().to_path_buf(),
            user,
            opts,
            issue,
            actor: ACTOR.into(),
            agent: "implementer".into(),
            base: "main".into(),
        })
        .unwrap();
        let followup = server
            .tools()
            .iter()
            .find(|tool| tool.name == "report_followup")
            .unwrap();
        let schema = to_tool(followup);
        let types = trk_agent::routing::RoutingConfig::new(&ConfigDir::with_user_dir(&seed, None))
            .types()
            .unwrap();
        assert_eq!(
            schema.input_schema["properties"]["type"]["enum"],
            json!(types.keys().collect::<Vec<_>>())
        );
        let description = schema.input_schema["properties"]["type"]["description"]
            .as_str()
            .unwrap();
        assert!(description.contains("default task"), "{description}");
        for (name, routing) in &types {
            assert!(
                description.contains(&format!("{name}: {}", routing.description)),
                "{description}"
            );
        }
        let (ok, out) = server.call_sync(
            "report_followup",
            json!({
                "title": "Unknown follow-up type",
                "description": "Must be rejected",
                "type": "unknown-type"
            }),
        );
        assert!(!ok, "{out}");
        assert_eq!(out["error"]["code"], "invalid_field");
        assert_eq!(
            out["error"]["allowed"],
            json!(types.keys().collect::<Vec<_>>())
        );
        assert!(
            out["error"]["hint"]
                .as_str()
                .unwrap()
                .contains(".trk/types/"),
            "{out}"
        );
        for (override_type, expected_type) in [(None, "task"), (Some("code-change"), "code-change")]
        {
            let mut args = json!({
                "title": "Follow-up issue",
                "description": "Work discovered during this session"
            });
            if let Some(issue_type) = override_type {
                args["type"] = json!(issue_type);
            }
            let (ok, out) = server.call_sync("report_followup", args);
            assert!(ok, "{out}");
            let created = out["result"]["created"].as_str().unwrap();
            let followup = store
                .read(None, |c| c.show(created, With::default()))
                .unwrap();
            assert_eq!(followup["type"], expected_type);
        }
    }

    #[test]
    fn restoration_restores_the_same_mcp_sessions_tools_and_lease_renewal() {
        for manual in [false, true] {
            for (ending, args, expected_state) in [
                (
                    "wait",
                    json!({"reason": "waiting again", "check": "exit 75"}),
                    State::Waiting,
                ),
                (
                    "close",
                    json!({"reason": "done", "note": "fixed the failed check"}),
                    State::Closed,
                ),
                (
                    "hand_back",
                    json!({"note": "need a different implementer"}),
                    State::Open,
                ),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let seed = dir.path().join("seed");
                write_seed(&seed);
                let db = dir.path().join("trk.db");
                let opts = StoreOpts {
                    now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
                    ..Default::default()
                };
                let mut store = Store::install(&db, opts.clone()).unwrap();
                let issue = store
                    .write(HUMAN, false, |c| {
                        c.create(NewIssue {
                            title: "Recover the waiting session".into(),
                            description: "A failed check needs its implementer again.".into(),
                            ..Default::default()
                        })
                    })
                    .unwrap();
                let original = store
                    .write(ACTOR, false, |c| {
                        c.claim(
                            &issue,
                            ClaimOpts {
                                terminal: Some("original-terminal".into()),
                                ..Default::default()
                            },
                        )
                    })
                    .unwrap();
                let mut server = TrkServer::new(McpArgs {
                    db,
                    config: ConfigDir::with_user_dir(&seed, None),
                    project_root: dir.path().to_path_buf(),
                    user: UserConfig::default(),
                    opts,
                    issue: issue.clone(),
                    actor: ACTOR.into(),
                    agent: "implementer".into(),
                    base: "main".into(),
                })
                .unwrap();
                let (ok, out) = server.call_sync(
                    "wait",
                    json!({"reason": "waiting for the check", "check": "exit 1"}),
                );
                assert!(ok, "{out}");
                assert_eq!(out["waiting"], issue);
                assert_released(&server, State::Waiting);
                server.renew_lease();
                assert!(active_claim(&store, &issue).is_none());
                let historical = store.read(None, |c| c.claims(&issue)).unwrap();
                assert_eq!(historical.len(), 1);
                assert_eq!(historical[0].id, original.id);
                assert_eq!(historical[0].release_reason.as_deref(), Some("waiting"));
                assert_eq!(historical[0].lease_until, original.lease_until);

                store.opts.now = Some("2026-11-20T10:05:00Z".parse().unwrap());
                let result = store
                    .write(HUMAN, false, |c| {
                        if manual {
                            let claim = c.resume(
                                &issue,
                                original.id,
                                c.issue_revision(&issue)?,
                                "SSH unlocked; continue",
                            )?;
                            return Ok(CheckResult::Woken { claim: claim.id });
                        }
                        c.check_record(
                            &issue,
                            "exit 1",
                            c.issue_revision(&issue)?,
                            &CheckRun {
                                exit: Some(1),
                                output: "merge failed".into(),
                                ..Default::default()
                            },
                            Some(original.id),
                        )
                    })
                    .unwrap();
                let restored = active_claim(&store, &issue).unwrap();
                assert_eq!(result, CheckResult::Woken { claim: restored.id });
                assert_ne!(restored.id, original.id);
                assert_eq!(restored.actor, ACTOR);
                assert!(restored.lease_until.is_some());

                // Advance only the existing server's clock: do not recreate its Session.
                let now = "2026-11-20T10:10:00Z".parse().unwrap();
                Arc::get_mut(&mut server.inner).unwrap().opts.now = Some(now);
                server.renew_lease();
                let renewed = active_claim(&store, &issue).unwrap();
                assert_eq!(renewed.id, restored.id);
                assert_eq!(
                    renewed.lease_until,
                    Some(trk_core::model::fmt_ts(now + store.opts.agent_lease))
                );
                assert!(renewed.lease_until > restored.lease_until);

                let (ok, out) = server.call_sync("add_note", json!({"body": "session resumed"}));
                assert!(ok, "{out}");
                assert_eq!(out["ok"], true);
                let (ok, out) = server.call_sync(ending, args);
                assert!(ok, "{ending}: {out}");
                assert_eq!(
                    store.read(None, |c| c.issue(&issue)).unwrap().state,
                    expected_state
                );
                assert!(active_claim(&store, &issue).is_none());
                assert_released(&server, expected_state);
            }
        }
    }

    #[test]
    fn stopped_mcp_restarts_after_failed_check_with_restored_actor_and_lease() {
        let dir = tempfile::tempdir().unwrap();
        let seed = dir.path().join("seed");
        write_seed(&seed);
        std::fs::write(
            seed.join("prompts/implementer.instructions.md.j2"),
            "claim={{ issue.claim.claimed_at }};state={{ issue.state }};actor={{ actor }}",
        )
        .unwrap();
        let db = dir.path().join("trk.db");
        let opts = StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            ..Default::default()
        };
        let mut store = Store::install(&db, opts.clone()).unwrap();
        let issue = store
            .write(HUMAN, false, |c| {
                c.create(NewIssue {
                    title: "Restart a dormant MCP server".into(),
                    ..Default::default()
                })
            })
            .unwrap();
        let original = store
            .write(ACTOR, false, |c| {
                c.claim(
                    &issue,
                    ClaimOpts {
                        terminal: Some("still-running-terminal".into()),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let args = |opts| McpArgs {
            db: db.clone(),
            config: ConfigDir::with_user_dir(&seed, None),
            project_root: dir.path().to_path_buf(),
            user: UserConfig::default(),
            opts,
            issue: issue.clone(),
            actor: ACTOR.into(),
            agent: "implementer".into(),
            base: "main".into(),
        };
        let server = TrkServer::new(args(opts.clone())).unwrap();
        server.record_started();
        let (ok, out) = server.call_sync(
            "wait",
            json!({"reason": "waiting for merge", "check": "exit 1"}),
        );
        assert!(ok, "{out}");
        server.patch_meta(json!({"mcp_exited_at": "2026-11-20T10:01:00Z"}));
        drop(server);
        assert!(active_claim(&store, &issue).is_none());
        let stopped = store.read(None, |c| c.actor_meta(ACTOR)).unwrap();
        assert_eq!(stopped["mcp_exited_at"], "2026-11-20T10:01:00Z");

        store.opts.now = Some("2026-11-20T10:05:00Z".parse().unwrap());
        let result = store
            .write(HUMAN, false, |c| {
                c.check_record(
                    &issue,
                    "exit 1",
                    c.issue_revision(&issue)?,
                    &CheckRun {
                        exit: Some(1),
                        output: "merge failed".into(),
                        ..Default::default()
                    },
                    Some(original.id),
                )
            })
            .unwrap();
        let restored = active_claim(&store, &issue).unwrap();
        assert_eq!(result, CheckResult::Woken { claim: restored.id });
        assert_ne!(restored.id, original.id);
        assert_eq!(restored.actor, ACTOR);
        assert_eq!(restored.terminal, original.terminal);
        assert_eq!(restored.terminal.as_deref(), Some("still-running-terminal"));
        assert_eq!(
            store.read(None, |c| c.actor_meta(ACTOR)).unwrap()["mcp_exited_at"],
            stopped["mcp_exited_at"]
        );

        let now = "2026-11-20T10:10:00Z".parse().unwrap();
        let server = TrkServer::new(args(StoreOpts {
            now: Some(now),
            ..opts
        }))
        .unwrap();
        assert_eq!(
            server.inner.instructions,
            format!("claim={};state=claimed;actor={ACTOR}", restored.claimed_at)
        );
        // Exercise the same startup marker and renewal tick as serve(), without
        // a real harness process, a wall-clock lease delay, or shared state.
        server.record_started();
        let started = store.read(None, |c| c.actor_meta(ACTOR)).unwrap();
        assert!(started["mcp_exited_at"].is_null());
        assert_eq!(started["mcp_pid"], std::process::id());
        assert!(started["mcp_started_at"].is_string());
        server.renew_lease();
        let renewed = active_claim(&store, &issue).unwrap();
        assert_eq!(renewed.id, restored.id);
        assert_eq!(renewed.actor, ACTOR);
        assert_eq!(
            renewed.lease_until,
            Some(trk_core::model::fmt_ts(now + store.opts.agent_lease))
        );
        assert!(renewed.lease_until > restored.lease_until);

        let (ok, out) =
            server.call_sync("add_note", json!({"body": "restarted MCP tool succeeds"}));
        assert!(ok, "{out}");
        assert_eq!(out["ok"], true);
        let (ok, out) = server.call_sync("get_task", json!({}));
        assert!(ok, "{out}");
        assert_eq!(out["claim"]["lease_until"], json!(renewed.lease_until));
        assert_eq!(out["claim"]["actor"], ACTOR);
        let history = store.read(None, |c| c.claims(&issue)).unwrap();
        assert_eq!(history.len(), 2);
    }
}
