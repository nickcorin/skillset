use clap::Parser;

#[derive(Parser)]
#[command(version, about, arg_required_else_help = true)]
pub struct Cli {}
