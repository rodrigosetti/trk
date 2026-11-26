use crate::Global;
use crate::env::Env;

pub(crate) fn serve(g: &Global, issue: &str, actor: &str, agent: &str) -> anyhow::Result<()> {
    let env = Env::new(g)?;
    trk_mcp::serve(trk_mcp::McpArgs {
        db: env.db.clone(),
        config: env.config.clone(),
        opts: env.store_opts()?,
        base: env.base(),
        project_root: env.root,
        user: env.user,
        issue: issue.to_string(),
        actor: actor.to_string(),
        agent: agent.to_string(),
    })
}
