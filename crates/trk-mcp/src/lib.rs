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
use trk_core::config::ConfigDir;
use trk_core::view::With;
use trk_core::{Store, StoreOpts};

pub struct McpArgs {
    pub db: PathBuf,
    pub config: ConfigDir,
    pub opts: StoreOpts,
    pub issue: String,
    pub actor: String,
    pub agent: String,
}

struct Inner {
    db: PathBuf,
    opts: StoreOpts,
    session: Session,
    agent: AgentDef,
    tools: Vec<ToolDef>,
    instructions: String,
}

#[derive(Clone)]
pub struct TrkServer {
    inner: Arc<Inner>,
}

const TASK_URI: &str = "trk://task";

impl TrkServer {
    /// Load the agent, generate its tools for the issue's type, render the
    /// instructions.
    pub fn new(args: McpArgs) -> anyhow::Result<TrkServer> {
        let store = Store::open(&args.db, args.opts.clone())?;
        let agent = trk_agent::load_agent(&args.config, &args.agent)?;
        let (tools, issue_json, worktree) = store.read(Some(&args.actor), |c| {
            let id = c.resolve_id(&args.issue)?;
            let issue = c.show(
                &id,
                With {
                    deps: true,
                    ..Default::default()
                },
            )?;
            let ty = c
                .schema
                .ty(issue["type"].as_str().unwrap_or_default())?
                .clone();
            let tools = trk_agent::agent_tools(&c.schema, &ty, &agent);
            let worktree = issue["claim"]["worktree"].as_str().map(String::from);
            Ok((tools, issue, worktree))
        })?;
        let input = trk_agent::prompt::PromptInput {
            issue: &issue_json,
            agent: &agent,
            tools: &tools,
            worktree: worktree.as_deref(),
            actor: &args.actor,
            harness: args.actor.split(':').nth(1),
        };
        let instructions = trk_agent::prompt::instructions(&args.config, &input)?;
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
        if self.inner.agent.inbox {
            let msgs = store.write(&self.inner.session.actor, false, |c| {
                if c.active_claim(&self.inner.session.issue)?
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
        let host = std::process::Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let s = server.clone();
        let started = json!({"mcp_pid": std::process::id(), "mcp_host": host, "mcp_started_at": trk_core::model::fmt_ts(jiff::Timestamp::now()), "mcp_exited_at": null});
        let _ = tokio::task::spawn_blocking(move || s.patch_meta(started)).await;

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
