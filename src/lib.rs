use anyhow::Result;
use clap::{Parser, Subcommand};

pub mod eligibility;
pub mod herdr;
pub mod jump;
pub mod keybindings;
pub mod model;

#[derive(Debug, Parser)]
#[command(
    name = "herdr-beacon",
    about = "Attention and activity-ordered agent navigation for Herdr"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    // Older hosts may still have a lifecycle hook queued during installation.
    // Accept it without any I/O; the manifest no longer registers hooks.
    #[command(hide = true)]
    Event,
    JumpUnread,
    JumpWorking,
    JumpRecent,
    JumpRecentReverse,
    InstallKeybindings,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Event => Ok(()),
        Commands::JumpUnread => {
            jump::navigate_from_environment(jump::NavigationMode::Unread).map(|_| ())
        }
        Commands::JumpRecentReverse => {
            jump::navigate_from_environment(jump::NavigationMode::RecentReverse).map(|_| ())
        }
        Commands::JumpRecent => {
            jump::navigate_from_environment(jump::NavigationMode::Recent).map(|_| ())
        }
        Commands::JumpWorking => {
            jump::navigate_from_environment(jump::NavigationMode::Working).map(|_| ())
        }
        Commands::InstallKeybindings => keybindings::install_from_environment().map(|_| ()),
    }
}
