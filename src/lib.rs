pub mod cli;
pub mod client;
pub mod commands;
pub mod config;
pub mod error;
pub mod output;
pub mod parse;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::error::exit_code;

/// Parses the command line, runs the command and returns the process exit code.
pub fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            // Help and version go to stdout and are not errors.
            return if e.use_stderr() { 1 } else { 0 };
        }
    };

    match dispatch(cli) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            exit_code(&e)
        }
    }
}

fn dispatch(cli: Cli) -> anyhow::Result<()> {
    let globals = cli.globals();
    match cli.command {
        Command::Function { cmd } => commands::function::run(&globals, cmd),
        Command::Composition { cmd } => commands::composition::run(&globals, cmd),
        Command::Invoke(args) => commands::invoke::run(&globals, args),
        Command::Config { cmd } => commands::config::run(&globals, cmd),
        Command::Ping => commands::ping::run(&globals),
        Command::Completions { shell } => {
            commands::completions::run(shell);
            Ok(())
        }
    }
}
