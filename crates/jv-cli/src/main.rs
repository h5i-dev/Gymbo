//! `jv` — an extremely fast JVM package and toolchain manager.
//!
//! The binary is thin on purpose. Everything it does lives in `jv-driver` and
//! the crates beneath it, so that the same work is reachable from a library and
//! testable without a subprocess.

mod args;
mod commands;

use clap::Parser;

use crate::args::{Cli, Command};

fn main() -> std::process::ExitCode {
    // The driver blocks on async work, so the session must not run on a tokio
    // worker thread. Keeping `main` synchronous is what guarantees that.
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Tree(args) => commands::tree(args),
        Command::Resolve(args) => commands::resolve(args),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

/// Prints an error and everything that caused it.
///
/// The chain matters: "cannot resolve a project" alone sends someone looking in
/// the wrong place, while the `checksum mismatch` three levels down is the
/// actual answer.
fn report(error: &anyhow::Error) {
    use owo_colors::OwoColorize;
    eprintln!("{} {error}", "error:".red().bold());
    for cause in error.chain().skip(1) {
        eprintln!("  {} {cause}", "caused by:".dimmed());
    }
}
