use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("version") => {
            if args.next().is_some() {
                eprintln!("usage: skillset version");
                return ExitCode::from(2);
            }
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!("unrecognized command `{command}`");
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
        None => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}

fn usage() -> &'static str {
    "\
usage: skillset <command>

commands:
  version    print the version this binary was built with"
}
