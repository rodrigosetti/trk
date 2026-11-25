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
        args.opts.chat_resume &= args.user.wait.chat_resume;
        args.opts.known_types = args
            .config
            .files_in("types", "toml")
            .into_iter()
            .filter(|path| path.is_file())
            .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        let store = Store::open(&args.db, args.opts.clone())?;
        let (issue_json, worktree, epics) = store.read(Some(&args.actor), |c| {
            let id = c.resolve_id(&args.issue)?;
            let issue = c.show(
                &id,
                With {
                    deps: true,
                    ..Default::default()
                },
            )?;
            let worktree = issue["claim"]["worktree"].as_str().map(String::from);
            Ok((issue, worktree, c.epics(false)?))
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
            let descriptions = epics
                .iter()
                .map(|epic| format!("{}: {}", epic.name, epic.description))
                .collect::<Vec<_>>()
                .join("; ");
            let names = epics
                .iter()
                .map(|epic| epic.name.as_str())
                .collect::<Vec<_>>();
            let field = &mut tool.input_schema["properties"]["epic"];
            field["anyOf"] = if epics.is_empty() {
                json!([{"pattern": "^\\s*$"}])
            } else {
                json!([{"enum": names}, {"pattern": "^\\s*$"}])
            };
            field["description"] = json!(format!(
                "Existing active epic name; omit or leave empty for no epic. Agents cannot create, rename, or archive epics. Active epics: {descriptions}"
            ));
        }
        let input = trk_agent::prompt::PromptInput {
            issue: &issue_json,
            epics: &epics,
            project_root: &args.project_root,
            config: &args.user,
            issue_type: &routing.issue_type,
            type_description: &routing.description,
            type_guidance: routing.guidance.as_deref(),
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
        for (path, content) in trk_core::seed::preset("local").unwrap() {
            let rendered = (path == "config.toml").then(|| {
                std::str::from_utf8(content)
                    .unwrap()
                    .replace("{{EMAIL_LINE}}", "email = \"test@example.com\"")
                    .replace("{{PREFIX}}", "trk")
                    .replace("{{LANDING}}", "\"merge\"")
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

    #[test]
    fn server_info_includes_type_guidance_with_an_agent_override() {
        let dir = tempfile::tempdir().unwrap();
        let seed = dir.path().join("seed");
        write_seed(&seed);
        std::fs::write(
            seed.join("types/change.toml"),
            "agent = 'implementer'\ndescription = 'Code change'\nworktree = false\nguidance = '''{% include \"prompts/type-guidance.md.j2\" %}'''",
        )
        .unwrap();
        std::fs::write(
            seed.join("prompts/type-guidance.md.j2"),
            "GUIDANCE {{ type.name }} {{ agent.name }} {{ issue.id }}",
        )
        .unwrap();
        let db = dir.path().join("trk.db");
        let opts = StoreOpts::default();
        let mut store = Store::install(&db, opts.clone()).unwrap();
        let issue = store
            .write(HUMAN, false, |c| {
                c.create(NewIssue {
                    title: "Guided MCP session".into(),
                    ..Default::default()
                })
            })
            .unwrap();
        let server = TrkServer::new(McpArgs {
            db,
            config: ConfigDir::with_user_dir(&seed, None),
            project_root: dir.path().to_path_buf(),
            user: UserConfig::default(),
            opts,
            issue: issue.clone(),
            actor: ACTOR.into(),
            agent: "researcher".into(),
            base: "main".into(),
        })
        .unwrap();
        let info = server.get_info();
        assert!(
            info.instructions
                .as_deref()
                .unwrap()
                .contains(&format!("GUIDANCE change researcher {issue}"))
        );
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
        for chat_resume in [true, false] {
            for check in [None, Some(" \n\t"), Some("exit 75")] {
                let mut user = UserConfig::default();
                user.wait.chat_resume = chat_resume;
                let (_dir, mut store, issue, server) = chat_resume_fixture(user);
                store
                    .write(ACTOR, false, |c| {
                        c.bind_terminal(&issue, "waiting-terminal")
                    })
                    .unwrap();
                let original = active_claim(&store, &issue).unwrap();
                let mut args = json!({"reason": "SQLite or Dolt?"});
                if let Some(check) = check {
                    args["check"] = json!(check);
                }
                let (ok, out) = server.call_sync("wait", args);
                assert!(ok, "{out}");
                assert_eq!(out["waiting"], issue);
                let message = out["message"].as_str().unwrap();
                let has_check = check.is_some_and(|s| !s.trim().is_empty());
                if has_check {
                    assert!(!message.contains("trk resume"), "{message}");
                } else {
                    assert!(
                        message.ends_with(&format!("trk resume {issue} -m \"<your decision>\"")),
                        "{message}"
                    );
                }
                assert_released(&server, State::Waiting);

                let (ok, out) = server.call_sync("resume", json!({"message": "Use SQLite."}));
                if has_check || !chat_resume {
                    assert!(!ok, "{out}");
                    assert_eq!(out["error"]["code"], "not_permitted");
                    assert_eq!(
                        store.read(None, |c| c.issue(&issue)).unwrap().state,
                        State::Waiting
                    );
                    assert!(active_claim(&store, &issue).is_none());
                    if has_check {
                        continue;
                    }
                    // Exercise the human operation behind the concrete CLI alternative.
                    store
                        .write(HUMAN, false, |c| {
                            c.resume(
                                &issue,
                                original.id,
                                c.issue_revision(&issue)?,
                                "Use SQLite.",
                            )
                        })
                        .unwrap();
                } else {
                    assert!(ok, "{out}");
                    assert_eq!(out["result"]["resumed"], issue);
                    assert_eq!(out["task"]["state"], "claimed");
                }
                assert_eq!(active_claim(&store, &issue).unwrap().actor, ACTOR);
                let (ok, out) = server.call_sync(
                    "close",
                    json!({"reason": "done", "note": "Implemented the chosen store."}),
                );
                assert!(ok, "{out}");
                assert_eq!(
                    store.read(None, |c| c.issue(&issue)).unwrap().state,
                    State::Closed
                );
            }
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
                    issue_type: Some("change".into()),
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
        for (override_type, expected_type) in [(None, "task"), (Some("change"), "change")] {
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
    fn followups_assign_only_existing_active_epics() {
        for has_epics in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let seed = dir.path().join("seed");
            write_seed(&seed);
            let db = dir.path().join("trk.db");
            let opts = StoreOpts::default();
            let mut store = Store::install(&db, opts.clone()).unwrap();
            let issue = store
                .write(HUMAN, false, |c| {
                    if has_epics {
                        c.create_epic("zulu", "Later delivery")?;
                        c.create_epic("alpha", "First delivery")?;
                        c.create_epic("retired", "Archived delivery")?;
                        c.archive_epic("retired")?;
                    }
                    c.create(NewIssue {
                        title: "Parent issue".into(),
                        epic: has_epics.then(|| "alpha".into()),
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
            let epic = &schema.input_schema["properties"]["epic"];
            assert_eq!(epic["type"], "string");
            assert!(
                !schema.input_schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("epic"))
            );
            let expected = if has_epics {
                json!([{"enum": ["alpha", "zulu"]}, {"pattern": "^\\s*$"}])
            } else {
                json!([{"pattern": "^\\s*$"}])
            };
            assert_eq!(epic["anyOf"], expected);
            let description = epic["description"].as_str().unwrap();
            assert!(description.contains("omit or leave empty"));
            assert!(!description.contains("Archived delivery"));
            if has_epics {
                assert!(description.contains("alpha: First delivery"));
                assert!(description.contains("zulu: Later delivery"));
            }
            let (ok, out) = server.call_sync("get_task", json!({}));
            assert!(ok, "{out}");
            assert_eq!(
                out["epic"],
                if has_epics {
                    json!("alpha")
                } else {
                    Value::Null
                }
            );

            let before = store.read(None, |c| c.epics(true)).unwrap();
            for name in ["unknown", "retired"] {
                let (ok, out) = server.call_sync(
                    "report_followup",
                    json!({
                        "title": "Invalid epic assignment",
                        "description": "Must not create an epic",
                        "epic": name
                    }),
                );
                assert!(!ok, "{out}");
                assert_eq!(out["error"]["code"], "invalid_field");
                assert_eq!(
                    out["error"]["allowed"],
                    if has_epics {
                        json!(["alpha", "zulu"])
                    } else {
                        json!([])
                    }
                );
                let hint = out["error"]["hint"].as_str().unwrap();
                assert!(hint.contains("allowed") && hint.contains("omit"), "{out}");
                assert_eq!(store.read(None, |c| c.epics(true)).unwrap(), before);
            }
            assert!(
                server
                    .tools()
                    .iter()
                    .all(|tool| !tool.name.contains("epic"))
            );
            for name in ["create_epic", "rename_epic", "archive_epic"] {
                let (ok, out) = server.call_sync(
                    name,
                    json!({"name": "alpha", "new": "renamed", "description": "Changed"}),
                );
                assert!(!ok, "{out}");
                assert_eq!(out["code"], "not_found");
                assert_eq!(store.read(None, |c| c.epics(true)).unwrap(), before);
            }
            let assignments = if has_epics {
                vec![None, Some(""), Some(" \n\t"), Some("zulu")]
            } else {
                vec![None, Some(""), Some(" \n\t")]
            };
            for assignment in assignments {
                let mut args = json!({
                    "title": "Follow-up issue",
                    "description": "Work discovered during this session"
                });
                if let Some(name) = assignment {
                    args["epic"] = json!(name);
                }
                let (ok, out) = server.call_sync("report_followup", args);
                assert!(ok, "{out}");
                let created = out["result"]["created"].as_str().unwrap();
                let followup = store
                    .read(None, |c| c.show(created, With::default()))
                    .unwrap();
                assert_eq!(
                    followup["epic"],
                    assignment
                        .filter(|name| !name.trim().is_empty())
                        .map_or(Value::Null, |name| json!(name))
                );
            }
        }
    }

    #[test]
    fn restoration_restores_the_same_mcp_sessions_tools_and_lease_renewal() {
        for restoration in ["check", "human", "chat"] {
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
                    if restoration == "chat" {
                        json!({"reason": "waiting for a human chat reply"})
                    } else {
                        json!({"reason": "waiting for the check", "check": "exit 1"})
                    },
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
                let message = "  Please continue.\nKeep the original scope.  \n";
                let result = if restoration == "chat" {
                    Arc::get_mut(&mut server.inner).unwrap().opts.now = store.opts.now;
                    let (ok, out) = server.call_sync("resume", json!({"message": message}));
                    assert!(ok, "{out}");
                    assert_eq!(out["ok"], true);
                    assert_eq!(out["result"]["resumed"], issue);
                    assert_eq!(out["task"]["state"], "claimed");
                    assert_eq!(out["task"]["claimed"], true);
                    assert_eq!(out["result"]["claim"]["actor"], ACTOR);
                    CheckResult::Woken {
                        claim: out["result"]["claim"]["id"].as_i64().unwrap(),
                    }
                } else {
                    store
                        .write(HUMAN, false, |c| {
                            if restoration == "human" {
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
                        .unwrap()
                };
                let restored = active_claim(&store, &issue).unwrap();
                assert_eq!(result, CheckResult::Woken { claim: restored.id });
                assert_ne!(restored.id, original.id);
                assert_eq!(restored.actor, ACTOR);
                assert!(restored.lease_until.is_some());
                assert_eq!(restored.terminal, original.terminal);
                if restoration == "chat" {
                    assert_eq!(store.read(None, |c| c.issue(&issue)).unwrap().wake_count, 0);
                    let events = store.read(None, |c| c.events(&issue)).unwrap();
                    let resumes: Vec<_> = events
                        .iter()
                        .filter(|event| event.op == "resume_chat")
                        .collect();
                    assert_eq!(resumes.len(), 1);
                    assert_eq!(resumes[0].actor, ACTOR);
                    assert_eq!(resumes[0].payload["message"], message);
                    assert_eq!(resumes[0].payload["resumed_from"], original.id);
                    assert_eq!(resumes[0].payload["claim"], restored.id);
                    assert!(!events.iter().any(|event| event.op == "resume"));
                    let notes = store.read(None, |c| c.notes(&issue)).unwrap();
                    let note = notes.last().unwrap();
                    assert_eq!(note["actor"], ACTOR);
                    assert_eq!(
                        note["body"],
                        format!("Session reported a human reply in chat:\n\n{message}")
                    );
                }

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

    fn chat_resume_fixture(user: UserConfig) -> (tempfile::TempDir, Store, String, TrkServer) {
        let dir = tempfile::tempdir().unwrap();
        let seed = dir.path().join("seed");
        write_seed(&seed);
        let db = dir.path().join("trk.db");
        let opts = StoreOpts::default();
        let mut store = Store::install(&db, opts.clone()).unwrap();
        let issue = store
            .write(HUMAN, false, |c| {
                c.create(NewIssue {
                    title: "Continue on a chat reply".into(),
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
            issue: issue.clone(),
            actor: ACTOR.into(),
            agent: "implementer".into(),
            base: "main".into(),
        })
        .unwrap();
        (dir, store, issue, server)
    }

    #[test]
    fn chat_resume_refuses_checked_waits_nonwaiting_and_other_sessions() {
        for scenario in ["checked", "claimed", "open", "closed", "different_session"] {
            let (_dir, store, issue, mut server) = chat_resume_fixture(UserConfig::default());
            match scenario {
                "claimed" => {}
                "open" => {
                    let (ok, out) = server.call_sync("hand_back", json!({"note": "released"}));
                    assert!(ok, "{out}");
                }
                "closed" => {
                    let (ok, out) =
                        server.call_sync("close", json!({"reason": "done", "note": "finished"}));
                    assert!(ok, "{out}");
                }
                _ => {
                    let mut args = json!({"reason": "waiting for a reply"});
                    if scenario == "checked" {
                        args["check"] = json!("exit 75");
                    }
                    let (ok, out) = server.call_sync("wait", args);
                    assert!(ok, "{out}");
                    if scenario == "different_session" {
                        Arc::get_mut(&mut server.inner).unwrap().session.actor =
                            "agent:omp:different-session".into();
                    }
                }
            }
            let before = store
                .read(None, |c| {
                    c.show(
                        &issue,
                        With {
                            notes: true,
                            events: true,
                            claims: true,
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
            let (ok, out) = server.call_sync("resume", json!({"message": "Please continue"}));
            assert!(!ok, "{scenario}: {out}");
            assert_eq!(out["error"]["code"], "not_permitted", "{scenario}: {out}");
            if scenario == "checked" {
                assert!(
                    out["error"]["hint"]
                        .as_str()
                        .unwrap()
                        .contains("trk resume")
                );
            }
            let after = store
                .read(None, |c| {
                    c.show(
                        &issue,
                        With {
                            notes: true,
                            events: true,
                            claims: true,
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
            assert_eq!(after, before, "{scenario}");
        }
    }

    #[test]
    fn chat_resume_obeys_disabled_user_configuration_even_with_default_store_options() {
        let mut user = UserConfig::default();
        user.wait.chat_resume = false;
        let (_dir, store, issue, server) = chat_resume_fixture(user);
        assert!(!server.inner.opts.chat_resume);
        let (ok, out) = server.call_sync("wait", json!({"reason": "a human must resume"}));
        assert!(ok, "{out}");
        let before = store.read(None, |c| c.events(&issue)).unwrap();
        let (ok, out) = server.call_sync("resume", json!({"message": "Please continue"}));
        assert!(!ok, "{out}");
        assert_eq!(out["error"]["code"], "not_permitted");
        assert_eq!(store.read(None, |c| c.events(&issue)).unwrap(), before);
        assert_eq!(
            store.read(None, |c| c.issue(&issue)).unwrap().state,
            State::Waiting
        );
        assert!(active_claim(&store, &issue).is_none());
    }

    #[test]
    fn chat_resume_requires_a_nonempty_message() {
        let (_dir, store, issue, server) = chat_resume_fixture(UserConfig::default());
        let (ok, out) = server.call_sync("wait", json!({"reason": "waiting for a human"}));
        assert!(ok, "{out}");
        let before = store.read(None, |c| c.events(&issue)).unwrap();
        for args in [
            json!({}),
            json!({"message": ""}),
            json!({"message": " \n\t "}),
            json!({"message": null}),
            json!({"message": 42}),
        ] {
            let (ok, out) = server.call_sync("resume", args.clone());
            assert!(!ok, "{args}: {out}");
            assert_eq!(out["error"]["code"], "bad_args", "{out}");
            assert_eq!(store.read(None, |c| c.events(&issue)).unwrap(), before);
            assert_eq!(
                store.read(None, |c| c.issue(&issue)).unwrap().state,
                State::Waiting
            );
            assert!(active_claim(&store, &issue).is_none());
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
