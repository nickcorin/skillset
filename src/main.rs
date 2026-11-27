mod cli;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => {
            if err.print().is_err() {
                return ExitCode::FAILURE;
            }
            u8::try_from(err.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from)
        }
    }
}
