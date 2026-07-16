//! Ilmari binary entry: dispatches CLI commands into the runtime or help output.
//!
//! Observer-only tmux radar for coding-agent panes. Feature gates (`tui`, `socket`,
//! `mcp`) control optional UI and publishing surfaces compiled into this binary.

mod agents;
mod app;
mod cli;
mod colors;
mod config;
mod daemon;
mod git;
mod ipc;
mod mcp;
mod model;
mod process;
#[cfg(feature = "tui")]
mod sound;
mod tmux;
mod tmux_state;
#[cfg(feature = "tui")]
mod ui;
mod view_state;

fn main() -> anyhow::Result<()> {
    match cli::parse_args(std::env::args().skip(1))? {
        cli::CliCommand::Run(config) => app::run(config),
        cli::CliCommand::DaemonStart(config) => daemon::start(config),
        cli::CliCommand::DaemonStop(config) => daemon::stop(config),
        cli::CliCommand::DaemonStatus(config) => daemon::daemon_status(config),
        cli::CliCommand::Status(config) => daemon::compact_status(config),
        cli::CliCommand::Help => {
            print!("{}", cli::help_text());
            Ok(())
        }
        cli::CliCommand::Version => {
            println!("{}", cli::version_text());
            Ok(())
        }
    }
}
