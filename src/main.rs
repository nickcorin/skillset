mod cli;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

const EXIT_FAILURE: u8 = 1;

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => ExitCode::from(write_clap_error(&err, &mut io::stdout(), &mut io::stderr())),
    }
}

fn write_clap_error(err: &clap::Error, stdout: &mut impl Write, stderr: &mut impl Write) -> u8 {
    let write_result = if err.use_stderr() {
        write!(stderr, "{}", err.render()).and_then(|()| stderr.flush())
    } else {
        write!(stdout, "{}", err.render()).and_then(|()| stdout.flush())
    };
    if write_result.is_err() {
        return EXIT_FAILURE;
    }
    u8::try_from(err.exit_code()).unwrap_or(EXIT_FAILURE)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use clap::Parser;

    use super::{Cli, write_clap_error};

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    #[test]
    fn write_failure_exits_one_for_version_and_help() {
        for flag in ["--version", "--help"] {
            let Err(err) = Cli::try_parse_from(["skillset", flag]) else {
                panic!("expected clap exit for {flag}");
            };
            let mut stderr = Vec::new();
            let code = write_clap_error(&err, &mut FailingWriter, &mut stderr);

            assert_eq!(code, 1, "expected exit 1 for {flag}");
            assert!(stderr.is_empty());
        }
    }

    #[test]
    fn version_writes_exact_bytes_to_stdout_and_exits_zero() {
        let Err(err) = Cli::try_parse_from(["skillset", "--version"]) else {
            panic!("expected clap version exit");
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = write_clap_error(&err, &mut stdout, &mut stderr);

        assert_eq!(code, 0);
        assert_eq!(
            stdout,
            format!("skillset {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn parse_error_writes_exact_bytes_to_stderr_and_exits_two() {
        let Err(err) = Cli::try_parse_from(["skillset", "--bogus"]) else {
            panic!("expected clap parse error");
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = write_clap_error(&err, &mut stdout, &mut stderr);

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(
            stderr,
            b"\
error: unexpected argument '--bogus' found

Usage: skillset

For more information, try '--help'.
"
        );
    }
}
