//! Handing the process over to the JVM.
//!
//! On Unix this does not spawn a child. It `exec`s, so the JVM *replaces* jvx in
//! the same process. That is not a micro-optimisation:
//!
//! - signals go where they are meant to. A `Ctrl-C` in a shell reaches the
//!   foreground process group, and with a wrapper in the middle the JVM's own
//!   shutdown hooks compete with a supervisor that has its own idea about
//!   `SIGINT`. `SIGTSTP`, `SIGWINCH` and terminal ownership all have the same
//!   shape of problem.
//! - the exit code is the JVM's by construction, rather than by a wrapper
//!   remembering to forward it.
//! - `ps` shows the tool, not a wrapper wrapping the tool.
//!
//! `CommandExt::exec` is a safe function — it can only fail, never succeed and
//! return — so this compiles under the workspace's `unsafe_code = "forbid"`
//! without a fallback. (`pre_exec` is the unsafe one; nothing here needs it.)
//! Windows has no equivalent, so there the process is spawned and waited on and
//! the code forwarded by hand.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use crate::error::ExecError;

/// A JVM invocation, ready to become the process.
#[derive(Clone, Debug)]
pub struct Launch {
    pub java: PathBuf,
    /// In classpath order: the tool's own jar first, then its dependencies in
    /// resolution order. The order decides which class wins when two jars carry
    /// the same one, so it is part of the contract, not a detail.
    pub class_path: Vec<PathBuf>,
    pub main_class: String,
    /// Everything after `--`, verbatim. jvx interprets none of it.
    pub args: Vec<String>,
}

impl Launch {
    /// The command this launch describes.
    ///
    /// Separate from [`Launch::run`] so that a test can assert on the argv
    /// without starting a JVM.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.java);
        command
            .arg("-cp")
            .arg(class_path(&self.class_path))
            .arg(&self.main_class)
            .args(&self.args);
        command
    }

    /// Runs the tool, replacing this process where the platform allows it.
    ///
    /// On Unix a successful call never returns. Everywhere else the JVM's exit
    /// code comes back.
    #[cfg(unix)]
    pub fn run(&self) -> Result<i32, ExecError> {
        use std::os::unix::process::CommandExt as _;

        let source = self.command().exec();
        Err(ExecError::Launch {
            java: self.java.clone(),
            source,
        })
    }

    #[cfg(not(unix))]
    pub fn run(&self) -> Result<i32, ExecError> {
        let status = self
            .command()
            .status()
            .map_err(|source| ExecError::Launch {
                java: self.java.clone(),
                source,
            })?;
        // A JVM killed by a signal has no code of its own; 1 is the least
        // surprising thing to report, and on Windows the case cannot arise.
        Ok(status.code().unwrap_or(1))
    }
}

/// Joins paths with the platform's classpath separator.
///
/// `OsString` rather than `String` because a cache directory is whatever the
/// filesystem says it is, and rendering it lossily would build a classpath that
/// points at nothing.
pub fn class_path(paths: &[PathBuf]) -> OsString {
    let separator = if cfg!(windows) { ";" } else { ":" };
    let mut joined = OsString::new();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            joined.push(separator);
        }
        joined.push(path);
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_joined_with_the_platform_separator() {
        let joined = class_path(&[PathBuf::from("/a/one.jar"), PathBuf::from("/b/two.jar")]);
        let expected = if cfg!(windows) {
            "/a/one.jar;/b/two.jar"
        } else {
            "/a/one.jar:/b/two.jar"
        };
        assert_eq!(joined, OsString::from(expected));
    }

    #[test]
    fn a_single_path_gains_no_separator() {
        assert_eq!(
            class_path(&[PathBuf::from("/a/one.jar")]),
            OsString::from("/a/one.jar")
        );
    }

    #[test]
    fn an_empty_classpath_is_empty() {
        assert_eq!(class_path(&[]), OsString::new());
    }

    #[test]
    fn the_command_puts_the_classpath_before_the_main_class() {
        // `java` stops reading options at the first non-option argument, so a
        // `-cp` after the main class would silently become an argument *to* the
        // tool and the classpath would be empty.
        let launch = Launch {
            java: PathBuf::from("/usr/bin/java"),
            class_path: vec![PathBuf::from("/a/one.jar")],
            main_class: "com.example.Main".to_owned(),
            args: vec!["--version".to_owned()],
        };
        let command = launch.command();
        let argv: Vec<_> = command.get_args().collect();
        assert_eq!(argv, ["-cp", "/a/one.jar", "com.example.Main", "--version"]);
    }

    #[test]
    fn tool_arguments_are_forwarded_verbatim() {
        // Including things that look like jvx's own flags; anything else would
        // make `jvx fmt -- --help` unable to reach the tool's help.
        let launch = Launch {
            java: PathBuf::from("java"),
            class_path: Vec::new(),
            main_class: "M".to_owned(),
            args: vec!["--help".to_owned(), "-".to_owned(), "a b".to_owned()],
        };
        let command = launch.command();
        let argv: Vec<_> = command.get_args().skip(3).collect();
        assert_eq!(argv, ["--help", "-", "a b"]);
    }
}
