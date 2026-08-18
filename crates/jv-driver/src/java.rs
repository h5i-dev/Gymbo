//! Finding out which Java is on this machine.
//!
//! POM profiles activate on `<jdk>`, and a great many published POMs use it —
//! Jackson, for one, gates its module descriptors that way. Without a
//! `java.version` those activators can never match, so jv would quietly build a
//! different effective model than Maven does on the same machine. That is
//! exactly the kind of divergence that is invisible until it isn't, so jv asks
//! the JDK rather than leaving the property unset.
//!
//! # Without starting a JVM
//!
//! Asking used to mean running `java -version`, which costs 35–40 ms because it
//! boots a JVM to print one line. On a warm `jv tree` that was the single largest
//! cost — more than resolution, parsing and rendering put together — and it is a
//! particularly bad one for a tool whose whole claim is not paying for JVM
//! startup.
//!
//! Every JDK since 9 ships a `release` file in its home directory containing
//! `JAVA_VERSION="21.0.11"`, which is the same string `java.version` reports.
//! Reading it costs a `stat` and a short read. The subprocess is still there for
//! a JDK 8, which has no `release` file.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `java.version` of the JDK jv would use, as the JVM spells it.
///
/// Looks at `JAVA_HOME` first, since that is what decides which JDK a Maven
/// build actually runs on, and falls back to whatever `java` is on the path.
/// Returns `None` when there is no JDK at all, which is a legitimate state: jv
/// can resolve dependencies without one, it just cannot match `<jdk>`
/// activators.
pub fn detect_version() -> Option<String> {
    // The cheap route first: a file, not a process.
    for home in homes() {
        if let Some(version) = read_release(&home) {
            return Some(version);
        }
    }
    for candidate in candidates() {
        if let Some(version) = probe(&candidate) {
            return Some(version);
        }
    }
    None
}

/// The JDK home directories to look in, most authoritative first.
fn homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        homes.push(PathBuf::from(home));
    }
    // The `java` on the path is usually a symlink into the real JDK — on Debian
    // two of them — so the home is found by resolving it and going up from
    // `bin/java`. Without the resolve this lands in `/usr/bin`, which has no
    // `release` file, and the probe would run after all.
    if let Some(executable) = executable() {
        let resolved = executable.canonicalize().unwrap_or(executable);
        if let Some(home) = resolved.parent().and_then(Path::parent) {
            homes.push(home.to_path_buf());
        }
    }
    homes
}

/// Reads `JAVA_VERSION` out of a JDK's `release` file.
///
/// The format is shell-style `KEY="value"` lines. Only this one key is wanted,
/// so the file is scanned rather than parsed.
fn read_release(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("JAVA_VERSION=") {
            let version = value.trim().trim_matches('"');
            if !version.is_empty() {
                return Some(version.to_owned());
            }
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
        .find(|candidate| is_executable(candidate))
}

/// Whether a path is a file this process could actually run.
///
/// `is_file()` alone is not enough: a non-executable file named `java` in any
/// writable directory on PATH would be selected here and then fail at launch,
/// while a shell would have skipped it and found the real one. That makes it a
/// denial of service anyone with write access to such a directory can arrange.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    // Windows has no execute bit; being a file is the whole test there.
    #[cfg(not(unix))]
    true
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
    fn a_non_executable_file_named_java_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let decoy = dir.path().join("java");
        std::fs::write(&decoy, b"not a program").unwrap();
        // A shell would skip this and keep looking; selecting it turns any
        // writable PATH directory into a denial of service for `jvx`.
        assert!(!is_executable(&decoy));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(is_executable(&decoy));
        }
        assert!(!is_executable(dir.path()));
        assert!(!is_executable(&dir.path().join("absent")));
    }

    #[test]
    fn a_release_file_is_read_without_starting_a_jvm() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("release"),
            "IMPLEMENTOR=\"Ubuntu\"\nJAVA_VERSION=\"21.0.11\"\nOS_ARCH=\"aarch64\"\n",
        )
        .unwrap();
        assert_eq!(read_release(dir.path()).as_deref(), Some("21.0.11"));
    }

    #[test]
    fn a_home_without_a_release_file_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // A JDK 8 has no `release` file, which is what the subprocess is still
        // there for.
        assert_eq!(read_release(dir.path()), None);

        std::fs::write(dir.path().join("release"), "IMPLEMENTOR=\"x\"\n").unwrap();
        assert_eq!(read_release(dir.path()), None);
    }

    #[test]
    fn an_early_access_release_file_keeps_its_spelling() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("release"), "JAVA_VERSION=\"24-ea\"\n").unwrap();
        assert_eq!(read_release(dir.path()).as_deref(), Some("24-ea"));
    }

    #[test]
    fn detection_agrees_with_the_jvm_it_describes() {
        // The point of reading the file is to get the same answer the subprocess
        // would. If a JDK is present, check that they actually agree, because a
        // silent disagreement changes which profiles activate.
        let Some(from_file) = homes().iter().find_map(|home| read_release(home)) else {
            eprintln!("no JDK with a release file; skipping");
            return;
        };
        let Some(from_jvm) = candidates().iter().find_map(probe) else {
            eprintln!("no runnable JDK; skipping");
            return;
        };
        assert_eq!(from_file, from_jvm);
    }

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
