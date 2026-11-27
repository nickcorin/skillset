mod cli;

use clap::Parser;

use crate::cli::Cli;

fn main() {
    Cli::parse();
}
