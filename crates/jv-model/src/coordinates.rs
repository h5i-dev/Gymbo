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

impl Default for Artifact {
    /// An empty artifact whose extension is still `jar`.
    ///
    /// Deriving this would leave the extension empty, which is not a value any
    /// Maven artifact has and would produce a file name ending in a bare dot.
    fn default() -> Self {
        Self {
            group_id: String::new(),
            artifact_id: String::new(),
            version: String::new(),
            classifier: String::new(),
            extension: DEFAULT_EXTENSION.to_owned(),
        }
    }
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
/// The suffix a declared snapshot ends with.
///
/// No leading dash, matching `Artifact.SNAPSHOT`. The dash is conventional, not
/// required, and Maven's own test is `endsWith("SNAPSHOT")`.
pub const SNAPSHOT: &str = "SNAPSHOT";

/// Whether a version string denotes a snapshot, in either spelling.
///
/// A declared snapshot ends in `SNAPSHOT`; a resolved one carries the deployment
/// timestamp and build number instead, as in `1.0-20240115.103000-7`.
///
/// Ported from `AbstractArtifact.isSnapshot` (maven-resolver), which is
/// `version.endsWith("SNAPSHOT")` or a match of
/// `^(.*-)?([0-9]{8}\.[0-9]{6}-[0-9]+)$`. Note the suffix test has **no**
/// leading dash: `1.0SNAPSHOT` is a snapshot to Maven.
pub fn is_snapshot_version(version: &str) -> bool {
    version.ends_with(SNAPSHOT) || timestamp_prefix_len(version).is_some()
}

/// Rewrites a resolved snapshot version back to its `SNAPSHOT` form.
///
/// Ported from `AbstractArtifact.toBaseVersion`. A version range is returned
/// unchanged — `[1.0,2.0)` is not a version and has no base — and a bare
/// timestamp with nothing in front of it becomes plain `SNAPSHOT`, because the
/// regex's prefix group is optional.
pub fn base_version_of(version: &str) -> String {
    // A range is not a version. Upstream checks this before the regex, and
    // without it `[1.0-20240115.103000-7]` would be rewritten into nonsense.
    if version.starts_with('[') || version.starts_with('(') {
        return version.to_owned();
    }
    match timestamp_prefix_len(version) {
        Some(prefix) => format!("{}{}", &version[..prefix], SNAPSHOT),
        None => version.to_owned(),
    }
}

/// The length of the `(.*-)?` prefix, if the version ends in a deployment
/// timestamp.
///
/// `Some(0)` is a real answer: it means the whole version *is* the timestamp,
/// whose base version is a bare `SNAPSHOT`.
///
/// Works on bytes rather than characters, which is both faster and the reason
/// this cannot panic. The pattern is entirely ASCII, so a byte that is part of a
/// multi-byte character can never match a digit or a delimiter — but slicing at
/// a fixed offset from the end *would* land inside one, and did: a version like
/// `1.0-日本語版-1` used to abort the resolve that read it.
fn timestamp_prefix_len(version: &str) -> Option<usize> {
    // `-[0-9]+` at the end.
    let bytes = version.as_bytes();
    let dash = bytes.iter().rposition(|byte| *byte == b'-')?;
    let build_number = &bytes[dash + 1..];
    if build_number.is_empty() || !build_number.iter().all(u8::is_ascii_digit) {
        return None;
    }

    // `[0-9]{8}\.[0-9]{6}` immediately before it.
    const TIMESTAMP: usize = 15;
    let timestamp_start = dash.checked_sub(TIMESTAMP)?;
    let timestamp = &bytes[timestamp_start..dash];
    let matches = timestamp[..8].iter().all(u8::is_ascii_digit)
        && timestamp[8] == b'.'
        && timestamp[9..].iter().all(u8::is_ascii_digit);
    if !matches {
        return None;
    }

    // The optional prefix must end in `-`, and only `-`. Accepting `.` or `_`
    // here turns `1.0.20240115.103000-7` — a perfectly ordinary release version
    // — into a snapshot, and jv would then look for it under a `1.0-SNAPSHOT`
    // directory that does not exist.
    match timestamp_start {
        0 => Some(0),
        _ if bytes[timestamp_start - 1] == b'-' => Some(timestamp_start),
        _ => None,
    }
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
    }

    #[test]
    fn a_version_with_multi_byte_characters_does_not_panic() {
        // These came off a POM someone published. Slicing at a fixed byte offset
        // from the end used to land inside a character and abort the resolve.
        for version in [
            "1.0.0-Ünïcödé-1",
            "1.0-日本語版-1",
            "日aaaaaaaaaaaaa-1",
            "\u{1f600}aaaaaaaaaaaa-1",
            "é-20240115.103000-7",
        ] {
            let _ = is_snapshot_version(version);
            let _ = base_version_of(version);
        }
        // And the accented prefix is still recognised correctly.
        assert!(is_snapshot_version("é-20240115.103000-7"));
        assert_eq!(base_version_of("é-20240115.103000-7"), "é-SNAPSHOT");
    }

    #[test]
    fn only_a_dash_may_precede_the_timestamp() {
        // Maven's regex prefix group is `(.*-)?`. Accepting `.` here would turn
        // an ordinary release into a snapshot and send jv looking for it under a
        // `-SNAPSHOT` directory that does not exist.
        assert!(!is_snapshot_version("1.0.20240115.103000-7"));
        assert!(!is_snapshot_version("1.0_20240115.103000-7"));
        assert_eq!(
            base_version_of("1.0.20240115.103000-7"),
            "1.0.20240115.103000-7"
        );
        assert!(is_snapshot_version("1.0-20240115.103000-7"));
    }

    #[test]
    fn a_bare_timestamp_has_a_bare_snapshot_base() {
        // The prefix group is optional, so the whole version can be a timestamp.
        assert!(is_snapshot_version("20240115.103000-7"));
        assert_eq!(base_version_of("20240115.103000-7"), "SNAPSHOT");
    }

    #[test]
    fn the_snapshot_suffix_does_not_require_a_dash() {
        // Maven's test is `endsWith("SNAPSHOT")`.
        assert!(is_snapshot_version("1.0SNAPSHOT"));
        assert!(is_snapshot_version("SNAPSHOT"));
        assert!(!is_snapshot_version("1.0-snapshot"));
    }

    #[test]
    fn a_range_is_not_rewritten() {
        // A range is not a version and has no base version; upstream checks this
        // before the pattern, and without it the rewrite produces nonsense.
        assert_eq!(base_version_of("[1.0,2.0)"), "[1.0,2.0)");
        assert_eq!(
            base_version_of("[1.0-20240115.103000-7]"),
            "[1.0-20240115.103000-7]"
        );
        assert_eq!(base_version_of("1.0"), "1.0");
        // Not a timestamp: too few digits.
        assert_eq!(
            base_version_of("1.0-2024015.10300-7"),
            "1.0-2024015.10300-7"
        );
    }
}
