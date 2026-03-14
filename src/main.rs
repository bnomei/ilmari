mod agents;
mod app;
mod colors;
mod git;
mod model;
mod process;
mod sound;
mod tmux;
mod ui;

fn main() -> anyhow::Result<()> {
    app::run()
}
