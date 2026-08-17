//! Artifact coordinates, dependency declarations, and the keys Maven groups
//! them by.

use std::fmt;

use crate::scope::Scope;
use crate::types::TypeRegistry;

/// The default `<type>` of a dependency.
pub const DEFAULT_TYPE: &str = "jar";
/// The default `<packaging>` of a project, and the extension of a plain jar.
pub const DEFAULT_EXTENSION: &str = "jar";

/// A repository-addressable artifact.
///
/// This is the identity a repository layout turns into a path: group, artifact,
/// version, classifier, extension. The declared `<type>` is deliberately absent
/// — it is a dependency-declaration concept that maps onto extension and
/// classifier (see [`TypeRegistry`]).
///
/// `version` is a plain string rather than a parsed [`jv_version::Version`]
/// because the same field carries a concrete version after resolution and,
/// before it, whatever was declared — including a range or a property that has
/// not been interpolated yet.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Artifact {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
    /// Empty when absent; Maven treats a missing and an empty classifier alike.
    pub classifier: String,
    pub extension: String,
}

impl Artifact {
    /// Builds a plain jar artifact.
    pub fn new(
        group_id: impl Into<String>,
        artifact_id: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            group_id: group_id.into(),
            artifact_id: artifact_id.into(),
            version: version.into(),
            classifier: String::new(),
            extension: DEFAULT_EXTENSION.to_owned(),
        }
    }

    pub fn with_classifier(mut self, classifier: impl Into<String>) -> Self {
        self.classifier = classifier.into();
        self
    }

    pub fn with_extension(mut self, extension: impl Into<String>) -> Self {
        self.extension = extension.into();
        self
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// The group and artifact alone: the unit Maven resolves version conflicts
    /// over.
    pub fn ga(&self) -> Ga<'_> {
        Ga {
            group_id: &self.group_id,
            artifact_id: &self.artifact_id,
        }
    }

    /// Whether the version is a `-SNAPSHOT` or a resolved snapshot timestamp.
    pub fn is_snapshot(&self) -> bool {
        is_snapshot_version(&self.version)
    }

    /// The base version: `1.0-SNAPSHOT` for a resolved `1.0-20240101.120000-3`,
    /// otherwise the version unchanged.
    ///
    /// Repository paths use the base version as the directory even when the file
    /// name carries a timestamp, so both spellings are needed.
    pub fn base_version(&self) -> String {
        base_version_of(&self.version)
    }

    /// The file name a Maven 2 layout repository uses for this artifact.
    pub fn file_name(&self) -> String {
        let mut name = format!("{}-{}", self.artifact_id, self.version);
        if !self.classifier.is_empty() {
            name.push('-');
            name.push_str(&self.classifier);
        }
        name.push('.');
        name.push_str(&self.extension);
        name
    }
}

impl fmt::Display for Artifact {
    /// Renders as `group:artifact:extension[:classifier]:version`, matching
    /// Maven Resolver's `DefaultArtifact` spelling.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.group_id, self.artifact_id, self.extension
        )?;
        if !self.classifier.is_empty() {
            write!(f, ":{}", self.classifier)?;
        }
        write!(f, ":{}", self.version)
    }
}

/// A group/artifact pair borrowed from an artifact or dependency.
///
/// Conflict resolution and `<dependencyManagement>` lookups both group by
/// identity without version, and doing so without allocating matters in the
/// resolver's inner loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ga<'a> {
    pub group_id: &'a str,
    pub artifact_id: &'a str,
}

impl fmt::Display for Ga<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.group_id, self.artifact_id)
    }
}

/// The key `<dependencyManagement>` is indexed by.
///
/// Management is keyed on type and classifier as well as identity, so a project
/// can manage `g:a:jar` and `g:a:test-jar` to different versions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ManagementKey {
    pub group_id: String,
    pub artifact_id: String,
    pub type_: String,
    pub classifier: String,
}

impl fmt::Display for ManagementKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group_id, self.artifact_id, self.type_)?;
        if !self.classifier.is_empty() {
            write!(f, ":{}", self.classifier)?;
        }
        Ok(())
    }
}

/// A `<dependency>` exactly as declared.
///
/// Fields that Maven distinguishes "absent" from "explicitly set" for are
/// `Option`, because `<dependencyManagement>` may only fill in what the
/// declaration left out.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Dependency {
    pub group_id: String,
    pub artifact_id: String,
    /// The declared version: a soft version, a range, or `None` when the
    /// declaration relies on management to supply one.
    pub version: Option<String>,
    /// The declared `<type>`, defaulting to `jar`.
    ///
    /// Kept as written because `dependency:tree` prints the type rather than the
    /// extension, and they differ for types like `test-jar`.
    pub type_: Option<String>,
    pub classifier: Option<String>,
    pub scope: Option<Scope>,
    pub optional: Option<bool>,
    pub exclusions: Vec<Exclusion>,
    /// Only meaningful for `system` scope.
    pub system_path: Option<String>,
}

impl Dependency {
    pub fn new(
        group_id: impl Into<String>,
        artifact_id: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            group_id: group_id.into(),
            artifact_id: artifact_id.into(),
            version: Some(version.into()),
            ..Default::default()
        }
    }

    /// The declared type, or `jar`.
    pub fn type_or_default(&self) -> &str {
        self.type_.as_deref().unwrap_or(DEFAULT_TYPE)
    }

    /// The declared classifier, or the empty string.
    pub fn classifier_or_default(&self) -> &str {
        self.classifier.as_deref().unwrap_or("")
    }

    /// The declared scope, or `compile`.
    pub fn scope_or_default(&self) -> Scope {
        self.scope.unwrap_or_default()
    }

    /// Whether the dependency is optional, defaulting to false.
    pub fn is_optional(&self) -> bool {
        self.optional.unwrap_or(false)
    }

    pub fn ga(&self) -> Ga<'_> {
        Ga {
            group_id: &self.group_id,
            artifact_id: &self.artifact_id,
        }
    }

    /// The key this declaration is managed under.
    pub fn management_key(&self) -> ManagementKey {
        ManagementKey {
            group_id: self.group_id.clone(),
            artifact_id: self.artifact_id.clone(),
            type_: self.type_or_default().to_owned(),
            classifier: self.classifier_or_default().to_owned(),
        }
    }

    /// Turns the declaration into a repository-addressable artifact.
    ///
    /// The type decides the extension and may supply a classifier; an explicitly
    /// declared classifier wins over the type's.
    pub fn to_artifact(&self, types: &TypeRegistry) -> Artifact {
        let type_name = self.type_or_default();
        let descriptor = types.get(type_name);
        let classifier = match self.classifier.as_deref() {
            Some(explicit) if !explicit.is_empty() => explicit.to_owned(),
            _ => descriptor.classifier.to_owned(),
        };
        Artifact {
            group_id: self.group_id.clone(),
            artifact_id: self.artifact_id.clone(),
            version: self.version.clone().unwrap_or_default(),
            classifier,
            extension: descriptor.extension.to_owned(),
        }
    }
}

impl fmt::Display for Dependency {
    /// Renders the way `dependency:tree` does:
    /// `group:artifact:type[:classifier]:version[:scope]`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.group_id,
            self.artifact_id,
            self.type_or_default()
        )?;
        let classifier = self.classifier_or_default();
        if !classifier.is_empty() {
            write!(f, ":{classifier}")?;
        }
        write!(f, ":{}", self.version.as_deref().unwrap_or("unknown"))?;
        if let Some(scope) = self.scope {
            write!(f, ":{scope}")?;
        }
        Ok(())
    }
}

/// An `<exclusion>`: a dependency subtree to prune.
///
/// `*` matches any group or artifact, so `*:*` excludes everything reachable
/// through the dependency it is attached to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Exclusion {
    pub group_id: String,
    pub artifact_id: String,
}

impl Exclusion {
    pub fn new(group_id: impl Into<String>, artifact_id: impl Into<String>) -> Self {
        Self {
            group_id: group_id.into(),
            artifact_id: artifact_id.into(),
        }
    }

    /// Whether this exclusion suppresses the given coordinates.
    pub fn matches(&self, group_id: &str, artifact_id: &str) -> bool {
        wildcard_match(&self.group_id, group_id) && wildcard_match(&self.artifact_id, artifact_id)
    }
}

impl fmt::Display for Exclusion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.group_id, self.artifact_id)
    }
}

/// Exclusion pattern matching: `*` is the only wildcard, and it matches wholly.
fn wildcard_match(pattern: &str, value: &str) -> bool {
    pattern == "*" || pattern == value
}

/// The suffix that marks a version as a snapshot.
pub const SNAPSHOT_SUFFIX: &str = "-SNAPSHOT";

/// Whether a version string denotes a snapshot, in either spelling.
///
/// A declared snapshot ends in `-SNAPSHOT`; a resolved one carries the
/// deployment timestamp and build number instead, as in
/// `1.0-20240115.103000-7`.
pub fn is_snapshot_version(version: &str) -> bool {
    version.ends_with(SNAPSHOT_SUFFIX) || timestamp_suffix_start(version).is_some()
}

/// Rewrites a resolved snapshot version back to its `-SNAPSHOT` form.
pub fn base_version_of(version: &str) -> String {
    match timestamp_suffix_start(version) {
        Some(start) => format!("{}{}", &version[..start], SNAPSHOT_SUFFIX),
        None => version.to_owned(),
    }
}

/// Finds where a `yyyyMMdd.HHmmss-buildNumber` suffix begins, if the version has
/// one.
///
/// Returns the index of the separator before the timestamp, so callers can both
/// strip it and rebuild the base version.
fn timestamp_suffix_start(version: &str) -> Option<usize> {
    // The build number follows the last '-'.
    let dash = version.rfind('-')?;
    let build_number = &version[dash + 1..];
    if build_number.is_empty() || !build_number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    // Before it sits `<8 digits>.<6 digits>`, itself preceded by a separator.
    let timestamp_end = dash;
    if timestamp_end < 15 {
        return None;
    }
    let timestamp = &version[timestamp_end - 15..timestamp_end];
    let (date, time) = timestamp.split_at(8);
    if date.bytes().all(|b| b.is_ascii_digit())
        && time.starts_with('.')
        && time[1..].bytes().all(|b| b.is_ascii_digit())
    {
        let separator = timestamp_end - 15;
        // A bare timestamp with nothing in front of it is not a snapshot
        // version, and the character in front must be a delimiter.
        if separator > 0 && matches!(version.as_bytes()[separator - 1], b'-' | b'.' | b'_') {
            return Some(separator - 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_file_names() {
        assert_eq!(Artifact::new("g", "a", "1.0").file_name(), "a-1.0.jar");
        assert_eq!(
            Artifact::new("g", "a", "1.0")
                .with_classifier("tests")
                .file_name(),
            "a-1.0-tests.jar"
        );
        assert_eq!(
            Artifact::new("g", "a", "1.0")
                .with_extension("pom")
                .file_name(),
            "a-1.0.pom"
        );
    }

    #[test]
    fn artifact_display_matches_resolver() {
        assert_eq!(Artifact::new("g", "a", "1.0").to_string(), "g:a:jar:1.0");
        assert_eq!(
            Artifact::new("g", "a", "1.0")
                .with_classifier("tests")
                .to_string(),
            "g:a:jar:tests:1.0"
        );
    }

    #[test]
    fn dependency_display_matches_tree_output() {
        let mut dep = Dependency::new("g", "a", "1.0");
        assert_eq!(dep.to_string(), "g:a:jar:1.0");
        dep.scope = Some(Scope::Test);
        assert_eq!(dep.to_string(), "g:a:jar:1.0:test");
        dep.type_ = Some("test-jar".to_owned());
        dep.classifier = Some("tests".to_owned());
        assert_eq!(dep.to_string(), "g:a:test-jar:tests:1.0:test");
    }

    #[test]
    fn management_key_ignores_version() {
        let a = Dependency::new("g", "a", "1.0").management_key();
        let b = Dependency::new("g", "a", "2.0").management_key();
        assert_eq!(a, b);
    }

    #[test]
    fn management_key_separates_types() {
        let jar = Dependency::new("g", "a", "1.0").management_key();
        let mut test_jar_dep = Dependency::new("g", "a", "1.0");
        test_jar_dep.type_ = Some("test-jar".to_owned());
        assert_ne!(jar, test_jar_dep.management_key());
    }

    #[test]
    fn exclusion_wildcards() {
        assert!(Exclusion::new("*", "*").matches("g", "a"));
        assert!(Exclusion::new("g", "*").matches("g", "a"));
        assert!(!Exclusion::new("g", "*").matches("h", "a"));
        assert!(Exclusion::new("g", "a").matches("g", "a"));
        assert!(!Exclusion::new("g", "a").matches("g", "b"));
        // Only a whole-field `*` is a wildcard; there is no prefix matching.
        assert!(!Exclusion::new("g.*", "a").matches("g.sub", "a"));
    }

    #[test]
    fn snapshot_detection() {
        assert!(is_snapshot_version("1.0-SNAPSHOT"));
        assert!(is_snapshot_version("1.0-20240115.103000-7"));
        assert!(!is_snapshot_version("1.0"));
        assert!(!is_snapshot_version("1.0-rc1"));
        // A build number alone is not a timestamped snapshot.
        assert!(!is_snapshot_version("1.0-7"));
    }

    #[test]
    fn base_version_normalizes_timestamps() {
        assert_eq!(base_version_of("1.0-20240115.103000-7"), "1.0-SNAPSHOT");
        assert_eq!(
            base_version_of("2.1.3-20240115.103000-142"),
            "2.1.3-SNAPSHOT"
        );
        assert_eq!(base_version_of("1.0-SNAPSHOT"), "1.0-SNAPSHOT");
        assert_eq!(base_version_of("1.0"), "1.0");
        // Not a timestamp: too few digits.
        assert_eq!(
            base_version_of("1.0-2024015.10300-7"),
            "1.0-2024015.10300-7"
        );
    }
}
