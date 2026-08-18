//! Which `java` to launch.
//!
//! The ladder — `JAVA_HOME/bin/java`, then `java` on `PATH` — is
//! `jv_driver::java`'s, and it stays there rather than being restated here so
//! that the JDK jv *resolves against* (its `java.version` decides which `<jdk>`
//! profiles activate) and the JDK jv *runs* can never drift apart.
//!
//! `ROADMAP.md` §3.7 puts provisioning a JDK when there is none at v0.3; until
//! then the answer to "no Java" is a clear error.

use std::path::PathBuf;

use crate::error::ExecError;

/// The `java` executable to run, or an error naming the two places jv looked.
pub fn locate() -> Result<PathBuf, ExecError> {
    jv_driver::java::executable().ok_or(ExecError::NoJava)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_with_java_on_its_path_finds_it() {
        // Not asserting a path: the point is that discovery agrees with the
        // driver's, and CI is the only place both are guaranteed present.
        match locate() {
            Ok(java) => assert!(java.is_file(), "{} should exist", java.display()),
            Err(error) => assert!(matches!(error, ExecError::NoJava), "{error}"),
        }
    }
}
