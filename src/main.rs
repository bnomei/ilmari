mod agents;
mod app;
mod cli;
mod colors;
mod git;
mod model;
mod process;
mod sound;
mod tmux;
mod ui;

fn main() -> anyhow::Result<()> {
    match cli::parse_args(std::env::args().skip(1))? {
        cli::CliCommand::Run(config) => app::run(config),
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
