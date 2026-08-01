use anyhow::Result;
use clap::{Parser, Subcommand};

pub mod event;
pub mod herdr;
pub mod jump;
pub mod keybindings;
pub mod model;
pub mod state;

#[derive(Debug, Parser)]
#[command(name = "herdr-beacon", about = "Unread agent navigation for Herdr")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Event,
    JumpUnread,
    InstallKeybindings,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Event => event::handle_event_from_environment(),
        Commands::JumpUnread => jump::jump_from_environment().map(|_| ()),
        Commands::InstallKeybindings => keybindings::install_from_environment().map(|_| ()),
    }
}
