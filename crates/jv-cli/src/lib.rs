//! What the `jv` and `jvx` binaries are both made of.
//!
//! Neither binary holds any logic of its own; both parse arguments into the same
//! types and hand them to the same functions. That is the point of the library:
//! `jvx <endpoint>` and `jv exec <endpoint>` cannot drift apart, because there
//! is only one of them.

pub mod args;
pub mod commands;
pub mod exec;

/// Prints an error and everything that caused it.
///
/// The chain matters: "cannot resolve a project" alone sends someone looking in
/// the wrong place, while the `checksum mismatch` three levels down is the
/// actual answer.
pub fn report_error(error: &anyhow::Error) {
    use owo_colors::OwoColorize as _;
    eprintln!("{} {error}", "error:".red().bold());
    for cause in error.chain().skip(1) {
        eprintln!("  {} {cause}", "caused by:".dimmed());
    }
}
