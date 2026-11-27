use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: skillset <command>

Commands:
  version    Print the version";

const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    run(env::args_os().skip(1))
}

fn run(mut args: impl Iterator<Item = OsString>) -> ExitCode {
    let Some(command) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(EXIT_USAGE);
    };

    match command.to_str() {
        Some("version") => {
            if let Some(extra) = args.next() {
                eprintln!(
                    "error: unexpected argument '{}' for 'version'\n\n{USAGE}",
                    extra.to_string_lossy()
                );
                return ExitCode::from(EXIT_USAGE);
            }
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "error: unknown command '{}'\n\n{USAGE}",
                command.to_string_lossy()
            );
            ExitCode::from(EXIT_USAGE)
        }
    }
}
