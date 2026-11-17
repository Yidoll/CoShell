use clap::{Parser, Subcommand};
use coshell_core::RuntimeService;

#[derive(Debug, Parser)]
#[command(name = "coshell", version, about = "CoShell command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report the shared runtime and component health.
    Health,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Health => println!(
            "{}",
            serde_json::to_string_pretty(&RuntimeService::new().health())?
        ),
    }
    Ok(())
}
