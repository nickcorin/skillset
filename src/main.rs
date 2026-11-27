use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the version this binary was built with
    Version,
}

struct VersionCommand<W> {
    version: &'static str,
    output: W,
}

impl<W: Write> VersionCommand<W> {
    fn print(mut self) -> io::Result<()> {
        writeln!(self.output, "{}", self.version)
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Version => VersionCommand {
            version: env!("CARGO_PKG_VERSION"),
            output: io::stdout(),
        }
        .print(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VersionCommand;

    #[test]
    fn version_command_writes_the_version_and_a_newline() {
        let mut output = Vec::new();
        VersionCommand {
            version: "1.2.3",
            output: &mut output,
        }
        .print()
        .unwrap();
        assert_eq!(output, b"1.2.3\n");
    }
}
