//! trk-agent: agent definitions, tool generation from a type's state machine,
//! the tool executor, prompt rendering, harness profiles and the launch spec.

pub mod agent;
pub mod check;
pub mod exec;
pub mod harness;
pub mod prompt;
pub mod tools;

pub use agent::{AgentDef, load_agent, load_agents};
pub use tools::{ToolDef, agent_tools, catalogue};
