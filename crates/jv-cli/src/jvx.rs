//! `jvx` — run a JVM tool straight from Maven coordinates.
//!
//! Exactly `jv exec` with the subcommand implied, the way `npx` is `npm exec`.
//! Both spellings exist because the short one is what anyone will type and the
//! long one is what works when only `jv` is on the path.

use std::process::ExitCode;

use clap::Parser;
use jv_cli::args::Jvx;
use jv_cli::{exec, report_error};

fn main() -> ExitCode {
    // Synchronous for the same reason `jv` is: the driver blocks on its own
    // runtime, which it may not do from a tokio worker thread.
    let cli = Jvx::parse();
    match exec::run(&cli.exec) {
        Ok(code) => code,
        Err(error) => {
            report_error(&error);
            ExitCode::FAILURE
        }
    }
}
