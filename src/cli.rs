use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(version, about, arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Restore every skill in skills-lock.json at the commit the lock records
    Install(InstallArgs),
}

#[derive(Args)]
pub struct InstallArgs {
    /// Lock file to restore from
    #[arg(long, value_name = "FILE", default_value = "skills-lock.json")]
    pub lock: PathBuf,

    /// Install into this folder instead of the lock's skillset.installDir or .agents/skills
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}
