mod cli;
mod tui;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    // Before the runtime starts any threads: this sets process environment variables.
    egin::load_env();
    // Two rustls crypto providers are compiled in through dependencies; wss:// needs one chosen.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = cli::Cli::parse();
    // A session's future nests the loop, a tool call and the sandbox call; unoptimised builds make
    // it big enough to overflow tokio's default 2 MiB worker stack under load.
    tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build()?.block_on(cli.run())
}
