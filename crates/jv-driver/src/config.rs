//! What a resolve was asked to do, and where its inputs live on this machine.
//!
//! Everything effectful that a resolve depends on — which `settings.xml` files
//! to read, where the cache is, whether to touch the network — is gathered here
//! so the rest of the driver takes it as data. That is what lets a test run the
//! whole pipeline against a temporary directory with no environment at all.

use std::path::{Path, PathBuf};

use jv_model::Settings;
use jv_repo::{UpdatePolicy, merge_settings};

use crate::error::DriverError;

/// How to run a resolve.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// `~/.m2/settings.xml` unless overridden. `Some` forces a specific file and
    /// makes its absence an error, which is what `-s` should do.
    pub user_settings: Option<PathBuf>,
    /// `$MAVEN_HOME/conf/settings.xml` unless overridden.
    pub global_settings: Option<PathBuf>,
    /// jv's own cache. Defaults to the platform cache directory.
    pub cache: Option<PathBuf>,
    /// Maven's `~/.m2/repository`, read but never written. `None` means "find
    /// it"; use [`Config::without_local_repository`] to opt out entirely.
    pub local_repository: Option<PathBuf>,
    /// Set when the caller explicitly asked for no `~/.m2` reads.
    pub ignore_local_repository: bool,
    /// Refuse to contact any repository.
    pub offline: bool,
    /// Forces an update policy on every repository, as Maven's `-U` does.
    pub update: Option<UpdatePolicy>,
    /// `-D` properties, which reach profile activation and interpolation.
    pub user_properties: Vec<(String, String)>,
    /// Profile ids forced on, as `-P id`.
    pub active_profiles: Vec<String>,
    /// Profile ids forced off, as `-P !id`. Beats every other rule.
    pub inactive_profiles: Vec<String>,
    /// The `java.version` that `<jdk>` profile activators match against.
    ///
    /// Detected from `JAVA_HOME` or `java` on the path when absent. Set it
    /// explicitly to resolve as a different JDK would — which is what makes a
    /// resolve reproducible across machines.
    pub java_version: Option<String>,
    /// Inject the plugins the packaging's lifecycle binds.
    ///
    /// Off by default, because resolving dependencies never reads
    /// `<build><plugins>` and `jv tree` would be paying for a list nothing looks
    /// at. `jv sync` turns it on: those plugins are artifacts a later `mvn -o`
    /// will demand from the local repository, and they appear in no POM.
    pub lifecycle_bindings: bool,
    /// The repositories to start from, replacing Maven Central.
    ///
    /// POMs may still add more as they are read; this only decides where the
    /// first request goes. Tests use it to resolve against a `file:` repository,
    /// and it is what a `--repository` flag would set.
    pub repositories: Option<Vec<jv_repo::Repository>>,
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Never reads Maven's local repository.
    ///
    /// Worth having for benchmarks and for reproducing a cold resolve, where a
    /// populated `~/.m2` would otherwise make jv look fast for the wrong reason.
    pub fn without_local_repository(mut self) -> Self {
        self.ignore_local_repository = true;
        self.local_repository = None;
        self
    }

    /// Reads and merges the settings files this config points at.
    ///
    /// A file the caller named explicitly must exist; a defaulted one that is
    /// absent simply contributes nothing, since most machines have no
    /// installation `settings.xml` and many have no user one either.
    pub fn load_settings(&self) -> Result<Settings, DriverError> {
        let user = match &self.user_settings {
            Some(path) => read_settings(path, true)?,
            None => default_user_settings()
                .map(|path| read_settings(&path, false))
                .transpose()?
                .flatten(),
        };
        let global = match &self.global_settings {
            Some(path) => read_settings(path, true)?,
            None => default_global_settings()
                .map(|path| read_settings(&path, false))
                .transpose()?
                .flatten(),
        };

        Ok(match (user, global) {
            (Some(user), Some(global)) => merge_settings(user, &global),
            (Some(only), None) | (None, Some(only)) => only,
            (None, None) => Settings::default(),
        })
    }
}

/// Reads one settings file. `required` decides whether absence is an error.
fn read_settings(path: &Path, required: bool) -> Result<Option<Settings>, DriverError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => {
            return Ok(None);
        }
        Err(source) => {
            return Err(DriverError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    jv_model::parse_settings(&text)
        .map(Some)
        .map_err(|source| DriverError::Settings {
            path: path.to_path_buf(),
            source,
        })
}

/// `~/.m2/settings.xml`.
fn default_user_settings() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".m2").join("settings.xml"))
}

/// `$MAVEN_HOME/conf/settings.xml`, under whichever of the two spellings the
/// machine uses. `M2_HOME` is the older one and is still what many CI images set.
fn default_global_settings() -> Option<PathBuf> {
    ["MAVEN_HOME", "M2_HOME"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|home| PathBuf::from(home).join("conf").join("settings.xml"))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, xml: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, xml).unwrap();
        path
    }

    #[test]
    fn a_named_settings_file_that_is_missing_is_an_error() {
        // Silently ignoring `-s /typo/settings.xml` would resolve against the
        // wrong repositories and look like it worked.
        let config = Config {
            user_settings: Some(PathBuf::from("/nonexistent/settings.xml")),
            ..Config::new()
        };
        assert!(matches!(
            config.load_settings(),
            Err(DriverError::Io { .. })
        ));
    }

    #[test]
    fn the_user_file_wins_over_the_installation_one() {
        let dir = tempfile::tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.xml",
            r#"<settings>
                 <localRepository>/opt/m2</localRepository>
                 <mirrors><mirror><id>m</id><url>https://old</url></mirror></mirrors>
               </settings>"#,
        );
        let user = write(
            dir.path(),
            "user.xml",
            r#"<settings>
                 <mirrors><mirror><id>m</id><url>https://new</url></mirror></mirrors>
               </settings>"#,
        );
        let config = Config {
            user_settings: Some(user),
            global_settings: Some(global),
            ..Config::new()
        };
        let settings = config.load_settings().unwrap();
        assert_eq!(settings.mirrors[0].url.as_deref(), Some("https://new"));
        assert_eq!(settings.local_repository.as_deref(), Some("/opt/m2"));
    }

    #[test]
    fn a_malformed_settings_file_is_reported_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "settings.xml", "<settings><mirrors>");
        let config = Config {
            user_settings: Some(path.clone()),
            ..Config::new()
        };
        match config.load_settings() {
            Err(DriverError::Settings { path: reported, .. }) => assert_eq!(reported, path),
            other => panic!("expected a settings error, got {other:?}"),
        }
    }
}
