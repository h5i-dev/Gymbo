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
    // `.mvn/maven.config` is spliced in before the real arguments, so a flag
    // given on the command line is parsed later and wins — Maven's precedence.
    // Without this a project that sets `-D` there resolves differently under jv
    // than under `mvn`, and nothing in either output says why.
    let argv = std::env::current_dir()
        .map(|directory| jv_driver::mvn_config::apply_to_command_line(std::env::args(), &directory))
        .unwrap_or_else(|_| std::env::args().collect());
    let cli = Cli::parse_from(argv);
    let result = match &cli.command {
        Command::Tree(args) => commands::tree(args).map(|()| ExitCode::SUCCESS),
        Command::Resolve(args) => commands::resolve(args).map(|()| ExitCode::SUCCESS),
        // `jv exec` reports the tool's exit code as its own, so unlike the other
        // subcommands it decides the code rather than merely succeeding.
        Command::Exec(args) => exec::run(args),
        Command::Sync(args) => commands::sync(args).map(|()| ExitCode::SUCCESS),
        Command::Profile(args) => commands::profile(args),
        Command::Add(args) => commands::add(args).map(|()| ExitCode::SUCCESS),
        Command::Remove(args) => commands::remove(args).map(|()| ExitCode::SUCCESS),
        Command::Outdated(args) => commands::outdated(args),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            report_error(&error);
            ExitCode::FAILURE
        }
    }
}
