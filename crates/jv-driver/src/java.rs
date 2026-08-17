//! Finding out which Java is on this machine.
//!
//! POM profiles activate on `<jdk>`, and a great many published POMs use it —
//! Jackson, for one, gates its module descriptors that way. Without a
//! `java.version` those activators can never match, so jv would quietly build a
//! different effective model than Maven does on the same machine. That is
//! exactly the kind of divergence that is invisible until it isn't, so jv asks
//! the JDK rather than leaving the property unset.

use std::path::PathBuf;
use std::process::Command;

/// The `java.version` of the JDK jv would use, as the JVM spells it.
///
/// Looks at `JAVA_HOME` first, since that is what decides which JDK a Maven
/// build actually runs on, and falls back to whatever `java` is on the path.
/// Returns `None` when there is no JDK at all, which is a legitimate state: jv
/// can resolve dependencies without one, it just cannot match `<jdk>`
/// activators.
pub fn detect_version() -> Option<String> {
    for candidate in candidates() {
        if let Some(version) = probe(&candidate) {
            return Some(version);
        }
    }
    None
}

/// The `java` executable jv would launch, or `None` when there is none.
///
/// Same ladder as [`detect_version`], and deliberately not the same
/// implementation: this one never starts a JVM. `jvx` is on a latency budget
/// measured against `npx`, and `java -version` costs the better part of a
/// hundred milliseconds — more than the whole warm path is allowed. So the
/// executable is found by looking at the filesystem, and whether it runs is
/// something the launch itself will report.
pub fn executable() -> Option<PathBuf> {
    if let Some(path) = java_home_executable() {
        if path.is_file() {
            return Some(path);
        }
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(EXECUTABLE))
        .find(|candidate| candidate.is_file())
}

/// The file name a JVM is installed under.
const EXECUTABLE: &str = if cfg!(windows) { "java.exe" } else { "java" };

fn java_home_executable() -> Option<PathBuf> {
    std::env::var_os("JAVA_HOME").map(|home| PathBuf::from(home).join("bin").join(EXECUTABLE))
}

fn candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = java_home_executable() {
        candidates.push(path);
    }
    // Bare, so the OS resolves it against PATH the way a shell would.
    candidates.push(PathBuf::from(EXECUTABLE));
    candidates
}

/// Runs `java -version` and reads the version out of it.
fn probe(java: &PathBuf) -> Option<String> {
    let output = Command::new(java).arg("-version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    // Every JVM writes `-version` to standard error, but reading both costs
    // nothing and survives a JVM that decides otherwise.
    parse_version(&String::from_utf8_lossy(&output.stderr))
        .or_else(|| parse_version(&String::from_utf8_lossy(&output.stdout)))
}

/// Extracts the version from `java -version` output.
///
/// The first line is `openjdk version "21.0.5" 2024-10-15` or, on an 8, `java
/// version "1.8.0_432"`. The quoted token is `java.version` verbatim, which is
/// what `<jdk>` ranges are written against — including the `1.8.0` spelling that
/// makes `[1.8,)` mean what its author intended.
fn parse_version(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|line| line.contains("version") && line.contains('"'))?;
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    let version = &line[start..end];
    (!version.is_empty()).then(|| version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modern_jdk_banner_parses() {
        let banner = "openjdk version \"21.0.5\" 2024-10-15\n\
                      OpenJDK Runtime Environment Temurin-21.0.5+11\n";
        assert_eq!(parse_version(banner).as_deref(), Some("21.0.5"));
    }

    #[test]
    fn a_java_8_banner_keeps_its_1_8_spelling() {
        // `<jdk>1.8</jdk>` and `[1.8,)` are both written against this exact
        // string; normalizing it to `8` would stop them matching.
        let banner = "java version \"1.8.0_432\"\n";
        assert_eq!(parse_version(banner).as_deref(), Some("1.8.0_432"));
    }

    #[test]
    fn an_early_access_build_parses() {
        let banner = "openjdk version \"24-ea\" 2025-03-18\n";
        assert_eq!(parse_version(banner).as_deref(), Some("24-ea"));
    }

    #[test]
    fn output_with_no_version_yields_nothing() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("Picked up JAVA_TOOL_OPTIONS: -Xmx1g\n"), None);
    }

    #[test]
    fn a_leading_noise_line_is_skipped() {
        // JVMs print this before the banner whenever JAVA_TOOL_OPTIONS is set,
        // which every CI image that tunes heap size does.
        let banner = "Picked up JAVA_TOOL_OPTIONS: -Xmx1g\n\
                      openjdk version \"17.0.13\" 2024-10-15\n";
        assert_eq!(parse_version(banner).as_deref(), Some("17.0.13"));
    }
}
