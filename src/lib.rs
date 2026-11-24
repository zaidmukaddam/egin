//! egin (Basque, "to do, to make"): a filesystem-first framework for durable backend agents.
//!
//! - [`agent`]: the agent folder (config, instructions, skills, profiles) and the loop that runs it
//! - [`model`]: the router client; [`tools`]: what the model can call (built-ins, web, MCP, documents)
//! - [`sandbox`]: where commands run (microsandbox, Daytona, Docker Sandboxes, local)
//! - [`store`]: SQLite sessions and event log; [`server`]: HTTP API, channels, the agent as an MCP server
//! - [`eval`]: eval runner, Terminal-Bench adapter, `evolve`, results page

// The agent loop's future nests the sandbox client's deeply enough that proving it Send passes the
// default limit of 128.
#![recursion_limit = "256"]

pub mod agent;
pub mod app;
pub mod atif;
pub mod catalog;
pub mod eval;
pub mod model;
pub mod sandbox;
pub mod server;
pub mod store;
pub mod telemetry;
pub mod tools;

pub use app::{load_env, new_id, App, Approve, Event};
pub use telemetry::Span;

/// An agent folder's state directory: sessions, the file index.
pub fn state_dir(agent_dir: &std::path::Path) -> std::path::PathBuf {
    agent_dir.join(".egin")
}

/// The session database in [`state_dir`].
pub fn state_db(agent_dir: &std::path::Path) -> std::path::PathBuf {
    state_dir(agent_dir).join("egin.db")
}

/// An `EGIN_<NAME>` setting from the environment.
pub fn setting(name: &str) -> Option<String> {
    std::env::var(format!("EGIN_{name}")).ok()
}
