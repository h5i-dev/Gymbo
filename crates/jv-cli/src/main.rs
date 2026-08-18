//! `jv` — an extremely fast JVM package and toolchain manager.
//!
//! The binary is thin on purpose. Everything it does lives in `jv-driver` and
//! the crates beneath it, so that the same work is reachable from a library and
//! testable without a subprocess.

use std::process::ExitCode;

use clap::Parser;
use jv_cli::args::{Cli, Command};
use jv_cli::{commands, exec, report_error};

fn main() -> ExitCode {
    // The driver blocks on async work, so the session must not run on a tokio
    // worker thread. Keeping `main` synchronous is what guarantees that.
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Tree(args) => commands::tree(args).map(|()| ExitCode::SUCCESS),
        Command::Resolve(args) => commands::resolve(args).map(|()| ExitCode::SUCCESS),
        // `jv exec` reports the tool's exit code as its own, so unlike the other
        // subcommands it decides the code rather than merely succeeding.
        Command::Exec(args) => exec::run(args),
        Command::Sync(args) => commands::sync(args).map(|()| ExitCode::SUCCESS),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            report_error(&error);
            ExitCode::FAILURE
        }
    }
}
