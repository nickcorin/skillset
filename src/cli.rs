use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "skillset", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the version
    Version,
}
