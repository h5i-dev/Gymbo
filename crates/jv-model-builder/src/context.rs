//! The inputs a model build depends on beyond the POMs themselves.
//!
//! Everything the environment contributes — command-line properties, JVM system
//! properties, which profiles the user forced on or off — arrives through
//! [`BuildContext`]. Profile activation and interpolation read only from here, so
//! both are deterministic functions of a context plus a POM, and a test can
//! pretend to be any JDK on any operating system.

use std::collections::BTreeMap;

/// System property names that profile activation depends on.
pub const OS_NAME: &str = "os.name";
pub const OS_ARCH: &str = "os.arch";
pub const OS_VERSION: &str = "os.version";
pub const JAVA_VERSION: &str = "java.version";

/// Command-line and environment inputs to a model build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildContext {
    /// Properties set on the command line (`-D`), the strongest source in
    /// interpolation after the model's own reflection.
    pub user_properties: BTreeMap<String, String>,
    /// JVM system properties, including environment variables under `env.`
    /// prefixes. `os.name`, `os.arch`, `os.version` and `java.version` here are
    /// what `<activation>` matches against.
    pub system_properties: BTreeMap<String, String>,
    /// Profile ids forced on with `-P id`.
    pub active_profiles: Vec<String>,
    /// Profile ids forced off with `-P !id`, which beats every other rule.
    pub inactive_profiles: Vec<String>,
}

impl BuildContext {
    /// An empty context: no properties, no forced profiles.
    ///
    /// Useful in tests, and honest for a resolve that must not depend on the
    /// machine it runs on.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Captures the current machine, spelling values the way a JVM would.
    ///
    /// The JVM spellings matter because POMs match against them literally: a
    /// project testing `<arch>amd64</arch>` expects the JVM's name for x86-64 on
    /// Linux, not Rust's `x86_64`.
    ///
    /// `java.version` is *not* filled in, because it depends on which JDK jv is
    /// about to use rather than on the machine. Callers that resolve a JDK should
    /// set it with [`BuildContext::with_java_version`]; without it, `<jdk>`
    /// activators cannot match.
    pub fn from_environment() -> Self {
        let mut system_properties = BTreeMap::new();
        system_properties.insert(OS_NAME.to_owned(), jvm_os_name().to_owned());
        system_properties.insert(OS_ARCH.to_owned(), jvm_os_arch().to_owned());
        if let Some(version) = os_version() {
            system_properties.insert(OS_VERSION.to_owned(), version);
        }
        // A POM interpolating `${user.home}` or `${line.separator}` is
        // interpolating a JVM system property, and Maven resolves it. jv has no
        // JVM, but these are all things the process already knows, and leaving
        // them literal put a `${...}` into a path where Maven puts a directory.
        // `java.version` is not here: it depends on which JDK the build will
        // use, which is the caller's to decide — see `with_java_version`.
        for (key, value) in [
            ("file.separator", std::path::MAIN_SEPARATOR_STR.to_owned()),
            (
                "path.separator",
                if cfg!(windows) { ";" } else { ":" }.to_owned(),
            ),
            (
                "line.separator",
                if cfg!(windows) { "\r\n" } else { "\n" }.to_owned(),
            ),
        ] {
            system_properties.insert(key.to_owned(), value);
        }
        for (key, value) in [
            ("user.home", dirs::home_dir()),
            ("user.dir", std::env::current_dir().ok()),
            ("java.home", std::env::var_os("JAVA_HOME").map(Into::into)),
        ] {
            if let Some(value) = value {
                system_properties.insert(key.to_owned(), value.to_string_lossy().into_owned());
            }
        }
        if let Some(user) = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME")) {
            system_properties.insert("user.name".to_owned(), user.to_string_lossy().into_owned());
        }

        // Maven exposes environment variables as `env.NAME` system properties,
        // which is how `<property><name>env.CI</name></property>` works.
        for (key, value) in std::env::vars() {
            system_properties.insert(format!("env.{key}"), value);
        }
        Self {
            system_properties,
            ..Default::default()
        }
    }

    /// Records which JDK the build will use, enabling `<jdk>` activation.
    pub fn with_java_version(mut self, version: impl Into<String>) -> Self {
        self.system_properties
            .insert(JAVA_VERSION.to_owned(), version.into());
        self
    }

    pub fn with_user_property(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.user_properties.insert(key.into(), value.into());
        self
    }

    pub fn with_system_property(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.system_properties.insert(key.into(), value.into());
        self
    }

    /// Looks a property up the way profile activation does: command line first,
    /// then system properties.
    pub fn property(&self, key: &str) -> Option<&str> {
        self.user_properties
            .get(key)
            .or_else(|| self.system_properties.get(key))
            .map(String::as_str)
    }
}

/// The JVM's `os.name` for the platform this binary was built for.
fn jvm_os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Mac OS X",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        "netbsd" => "NetBSD",
        other => other,
    }
}

/// The JVM's `os.arch`, which differs from Rust's naming and, for x86-64, differs
/// between operating systems: Linux JVMs report `amd64` where macOS JVMs report
/// `x86_64`.
fn jvm_os_arch() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "macos") => "x86_64",
        ("x86_64", _) => "amd64",
        ("aarch64", _) => "aarch64",
        ("x86", _) => "x86",
        ("arm", _) => "arm",
        ("powerpc64", _) => "ppc64",
        ("riscv64", _) => "riscv64",
        (other, _) => other,
    }
}

/// The kernel release, as the JVM's `os.version` reports it.
///
/// Read from `/proc` on Linux, which costs nothing. On other Unixes it needs
/// `uname`, so this spawns a process — acceptable because it happens once per
/// invocation, and only relevant to the rare POM with an `<os><version>`
/// activator. A caller that cannot afford it can build the context by hand.
fn os_version() -> Option<String> {
    if cfg!(target_os = "linux") {
        if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            return Some(release.trim().to_owned());
        }
    }
    let output = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!version.is_empty()).then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context_has_nothing() {
        let context = BuildContext::empty();
        assert!(context.property("os.name").is_none());
        assert!(context.active_profiles.is_empty());
    }

    #[test]
    fn user_properties_beat_system_properties() {
        let context = BuildContext::empty()
            .with_system_property("env", "dev")
            .with_user_property("env", "prod");
        assert_eq!(context.property("env"), Some("prod"));
    }

    #[test]
    fn environment_capture_uses_jvm_spellings() {
        let context = BuildContext::from_environment();
        let name = context.property(OS_NAME).expect("os.name");
        let arch = context.property(OS_ARCH).expect("os.arch");
        // Rust's names must not leak through.
        assert_ne!(name, "linux");
        assert_ne!(name, "macos");
        assert_ne!(arch, "x86_64_linux");
        if cfg!(target_os = "linux") {
            assert_eq!(name, "Linux");
            assert!(context.property(OS_VERSION).is_some());
        }
        if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            assert_eq!(arch, "amd64");
        }
        // java.version depends on the JDK jv picks, not on the machine.
        assert!(context.property(JAVA_VERSION).is_none());
    }

    #[test]
    fn environment_variables_are_exposed_with_a_prefix() {
        // PATH is set in every environment this runs in.
        let context = BuildContext::from_environment();
        assert!(context.property("env.PATH").is_some());
        assert!(context.property("PATH").is_none());
    }

    #[test]
    fn java_version_is_opt_in() {
        let context = BuildContext::empty().with_java_version("21.0.11");
        assert_eq!(context.property(JAVA_VERSION), Some("21.0.11"));
    }
}
