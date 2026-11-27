mod cli;
mod commands;

use std::io;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::commands::version::VersionPrinter;

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Version => {
            VersionPrinter::new(env!("CARGO_PKG_VERSION"), io::stdout().lock()).print()
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
