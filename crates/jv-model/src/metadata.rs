//! `maven-metadata.xml`: what versions a repository holds.
//!
//! Mirrors `api/maven-api-metadata/src/main/mdo/metadata.mdo`.
//!
//! The same file name serves three different purposes depending on where it
//! sits, which is the main thing to keep straight:
//!
//! - under `group/artifact/` it lists the released versions, and names the
//!   `latest` and `release` ones, which is what resolves `RELEASE` and a version
//!   range's candidates;
//! - under `group/artifact/version-SNAPSHOT/` it says which timestamped build
//!   is current, which is what turns `1.0-SNAPSHOT` into
//!   `1.0-20240115.103000-7`;
//! - under `group/` it maps plugin prefixes to artifact ids, which jv does not
//!   need yet.
//!
//! Corrupt metadata must be *ignored*, not fatal. Repositories serve truncated
//! files and HTML error pages under this name, and Maven treats an unparseable
//! `maven-metadata.xml` as though the file were absent (its own regression test
//! for this, MNG-4498, ships a deliberately truncated one). Callers in `jv-repo`
//! are expected to degrade the same way rather than failing a resolve.

use crate::parse::{ParseError, XmlParser};

/// A parsed `maven-metadata.xml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub model_version: Option<String>,
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    /// Present only in per-snapshot metadata, naming the `-SNAPSHOT` version the
    /// file describes.
    pub version: Option<String>,
    pub versioning: Option<Versioning>,
    pub plugins: Vec<PluginMapping>,
}

impl Metadata {
    /// The versions this metadata lists, newest-last as written.
    ///
    /// Order is *not* trustworthy: repositories append on deploy, so a version
    /// published out of order stays out of order. Callers that need the greatest
    /// version must compare rather than take the last.
    pub fn versions(&self) -> &[String] {
        self.versioning
            .as_ref()
            .map_or(&[], |versioning| &versioning.versions)
    }

    /// The version `RELEASE` refers to, if the repository states one.
    pub fn release(&self) -> Option<&str> {
        self.versioning
            .as_ref()
            .and_then(|versioning| versioning.release.as_deref())
    }

    /// The version `LATEST` refers to, if the repository states one.
    pub fn latest(&self) -> Option<&str> {
        self.versioning
            .as_ref()
            .and_then(|versioning| versioning.latest.as_deref())
    }

    /// Expands a `-SNAPSHOT` version into the concrete timestamped version for a
    /// given extension and classifier.
    ///
    /// Prefers an exact `<snapshotVersion>` entry, since a repository may hold
    /// different builds per classifier, and falls back to composing the version
    /// from `<snapshot>`'s timestamp and build number the way Maven does when
    /// only that is present.
    ///
    /// # Examples
    ///
    /// ```
    /// use jv_model::parse_metadata;
    ///
    /// let metadata = parse_metadata(r#"
    ///     <metadata>
    ///       <groupId>g</groupId><artifactId>a</artifactId><version>1.0-SNAPSHOT</version>
    ///       <versioning>
    ///         <snapshot><timestamp>20240115.103000</timestamp><buildNumber>7</buildNumber></snapshot>
    ///       </versioning>
    ///     </metadata>
    /// "#).unwrap();
    ///
    /// assert_eq!(
    ///     metadata.snapshot_version("jar", "").as_deref(),
    ///     Some("1.0-20240115.103000-7")
    /// );
    /// ```
    pub fn snapshot_version(&self, extension: &str, classifier: &str) -> Option<String> {
        let versioning = self.versioning.as_ref()?;

        if let Some(exact) = versioning.snapshot_versions.iter().find(|candidate| {
            candidate.extension.as_deref().unwrap_or("") == extension
                && candidate.classifier.as_deref().unwrap_or("") == classifier
        }) {
            return exact.value.clone();
        }

        let snapshot = versioning.snapshot.as_ref()?;
        let timestamp = snapshot.timestamp.as_deref()?;
        let build_number = snapshot.build_number.as_deref()?;
        // The base version is the `-SNAPSHOT` one this file describes.
        let base = self.version.as_deref()?;
        let stem = base.strip_suffix("-SNAPSHOT")?;
        Some(format!("{stem}-{timestamp}-{build_number}"))
    }

    /// Whether the snapshot lives only in the local repository, and so must not
    /// be treated as something a remote repository could supply.
    pub fn is_local_copy(&self) -> bool {
        self.versioning
            .as_ref()
            .and_then(|versioning| versioning.snapshot.as_ref())
            .and_then(|snapshot| snapshot.local_copy)
            .unwrap_or(false)
    }
}

/// The `<versioning>` block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Versioning {
    pub latest: Option<String>,
    pub release: Option<String>,
    pub versions: Vec<String>,
    /// A `yyyyMMddHHmmss` stamp, in UTC.
    pub last_updated: Option<String>,
    pub snapshot: Option<Snapshot>,
    pub snapshot_versions: Vec<SnapshotVersion>,
}

/// The current build of a snapshot version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// A `yyyyMMdd.HHmmss` stamp, as it appears inside version strings.
    pub timestamp: Option<String>,
    pub build_number: Option<String>,
    /// True when the snapshot was installed locally rather than deployed.
    pub local_copy: Option<bool>,
}

/// One artifact of a snapshot build, since classifiers may differ in version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotVersion {
    pub classifier: Option<String>,
    pub extension: Option<String>,
    /// The resolved, timestamped version.
    pub value: Option<String>,
    pub updated: Option<String>,
}

/// A plugin prefix mapping, from group-level metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginMapping {
    pub name: Option<String>,
    /// The short prefix, as in the `dependency` of `mvn dependency:tree`.
    pub prefix: Option<String>,
    pub artifact_id: Option<String>,
}

/// Parses a `maven-metadata.xml`.
pub fn parse_metadata(xml: &str) -> Result<Metadata, ParseError> {
    let mut parser = XmlParser::new(xml);
    parser.enter_root("metadata")?;
    let mut metadata = Metadata::default();
    parser.children("metadata", |parser, name| match name {
        b"modelVersion" => parser.text_into(&mut metadata.model_version),
        b"groupId" => parser.text_into(&mut metadata.group_id),
        b"artifactId" => parser.text_into(&mut metadata.artifact_id),
        b"version" => parser.text_into(&mut metadata.version),
        b"versioning" => {
            metadata.versioning = Some(parse_versioning(parser)?);
            Ok(true)
        }
        b"plugins" => {
            metadata.plugins = parser.list("plugins", b"plugin", parse_plugin_mapping)?;
            Ok(true)
        }
        _ => Ok(false),
    })?;
    Ok(metadata)
}

fn parse_versioning(parser: &mut XmlParser<'_>) -> Result<Versioning, ParseError> {
    let mut versioning = Versioning::default();
    parser.children("versioning", |parser, name| match name {
        b"latest" => parser.text_into(&mut versioning.latest),
        b"release" => parser.text_into(&mut versioning.release),
        b"versions" => {
            versioning.versions = parser.text_list("versions", b"version")?;
            Ok(true)
        }
        b"lastUpdated" => parser.text_into(&mut versioning.last_updated),
        b"snapshot" => {
            versioning.snapshot = Some(parse_snapshot(parser)?);
            Ok(true)
        }
        b"snapshotVersions" => {
            versioning.snapshot_versions = parser.list(
                "snapshotVersions",
                b"snapshotVersion",
                parse_snapshot_version,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    })?;
    Ok(versioning)
}

fn parse_snapshot(parser: &mut XmlParser<'_>) -> Result<Snapshot, ParseError> {
    let mut snapshot = Snapshot::default();
    parser.children("snapshot", |parser, name| match name {
        b"timestamp" => parser.text_into(&mut snapshot.timestamp),
        b"buildNumber" => parser.text_into(&mut snapshot.build_number),
        b"localCopy" => parser.bool_into(&mut snapshot.local_copy),
        _ => Ok(false),
    })?;
    Ok(snapshot)
}

fn parse_snapshot_version(parser: &mut XmlParser<'_>) -> Result<SnapshotVersion, ParseError> {
    let mut version = SnapshotVersion::default();
    parser.children("snapshotVersion", |parser, name| match name {
        b"classifier" => parser.text_into(&mut version.classifier),
        b"extension" => parser.text_into(&mut version.extension),
        b"value" => parser.text_into(&mut version.value),
        b"updated" => parser.text_into(&mut version.updated),
        _ => Ok(false),
    })?;
    Ok(version)
}

fn parse_plugin_mapping(parser: &mut XmlParser<'_>) -> Result<PluginMapping, ParseError> {
    let mut mapping = PluginMapping::default();
    parser.children("plugin", |parser, name| match name {
        b"name" => parser.text_into(&mut mapping.name),
        b"prefix" => parser.text_into(&mut mapping.prefix),
        b"artifactId" => parser.text_into(&mut mapping.artifact_id),
        _ => Ok(false),
    })?;
    Ok(mapping)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_metadata() {
        let metadata = parse_metadata(
            r#"<metadata>
                 <groupId>org.slf4j</groupId>
                 <artifactId>slf4j-api</artifactId>
                 <versioning>
                   <latest>2.1.0-alpha1</latest>
                   <release>2.0.9</release>
                   <versions>
                     <version>1.7.36</version>
                     <version>2.0.9</version>
                     <version>2.1.0-alpha1</version>
                   </versions>
                   <lastUpdated>20240115103000</lastUpdated>
                 </versioning>
               </metadata>"#,
        )
        .unwrap();

        assert_eq!(metadata.group_id.as_deref(), Some("org.slf4j"));
        assert_eq!(metadata.release(), Some("2.0.9"));
        assert_eq!(metadata.latest(), Some("2.1.0-alpha1"));
        assert_eq!(metadata.versions().len(), 3);
        assert_eq!(
            metadata
                .versioning
                .as_ref()
                .unwrap()
                .last_updated
                .as_deref(),
            Some("20240115103000")
        );
        // No snapshot section, so nothing to expand.
        assert_eq!(metadata.snapshot_version("jar", ""), None);
    }

    #[test]
    fn snapshot_metadata_composed_from_timestamp() {
        let metadata = parse_metadata(
            r#"<metadata>
                 <groupId>g</groupId><artifactId>a</artifactId><version>1.0-SNAPSHOT</version>
                 <versioning>
                   <snapshot>
                     <timestamp>20240115.103000</timestamp>
                     <buildNumber>7</buildNumber>
                   </snapshot>
                 </versioning>
               </metadata>"#,
        )
        .unwrap();

        assert_eq!(
            metadata.snapshot_version("jar", "").as_deref(),
            Some("1.0-20240115.103000-7")
        );
        // Composition is classifier-independent when only <snapshot> is present.
        assert_eq!(
            metadata.snapshot_version("jar", "sources").as_deref(),
            Some("1.0-20240115.103000-7")
        );
        assert!(!metadata.is_local_copy());
    }

    #[test]
    fn snapshot_versions_win_over_composition() {
        let metadata = parse_metadata(
            r#"<metadata>
                 <groupId>g</groupId><artifactId>a</artifactId><version>1.0-SNAPSHOT</version>
                 <versioning>
                   <snapshot><timestamp>20240115.103000</timestamp><buildNumber>7</buildNumber></snapshot>
                   <snapshotVersions>
                     <snapshotVersion>
                       <extension>jar</extension>
                       <value>1.0-20240115.103000-7</value>
                     </snapshotVersion>
                     <snapshotVersion>
                       <classifier>sources</classifier>
                       <extension>jar</extension>
                       <value>1.0-20240110.090000-6</value>
                     </snapshotVersion>
                     <snapshotVersion>
                       <extension>pom</extension>
                       <value>1.0-20240115.103000-7</value>
                     </snapshotVersion>
                   </snapshotVersions>
                 </versioning>
               </metadata>"#,
        )
        .unwrap();

        assert_eq!(
            metadata.snapshot_version("jar", "").as_deref(),
            Some("1.0-20240115.103000-7")
        );
        // A classifier may lag behind the main artifact, which is exactly why the
        // exact entry has to win over composing from <snapshot>.
        assert_eq!(
            metadata.snapshot_version("jar", "sources").as_deref(),
            Some("1.0-20240110.090000-6")
        );
        assert_eq!(
            metadata.snapshot_version("pom", "").as_deref(),
            Some("1.0-20240115.103000-7")
        );
        // An extension the repository does not hold falls back to composition.
        assert_eq!(
            metadata.snapshot_version("war", "").as_deref(),
            Some("1.0-20240115.103000-7")
        );
    }

    #[test]
    fn local_copy_is_flagged() {
        let metadata = parse_metadata(
            r#"<metadata><versioning>
                 <snapshot><localCopy>true</localCopy></snapshot>
               </versioning></metadata>"#,
        )
        .unwrap();
        assert!(metadata.is_local_copy());
    }

    #[test]
    fn plugin_prefix_mappings() {
        let metadata = parse_metadata(
            r#"<metadata>
                 <groupId>org.apache.maven.plugins</groupId>
                 <plugins>
                   <plugin>
                     <name>Apache Maven Dependency Plugin</name>
                     <prefix>dependency</prefix>
                     <artifactId>maven-dependency-plugin</artifactId>
                   </plugin>
                 </plugins>
               </metadata>"#,
        )
        .unwrap();
        assert_eq!(metadata.plugins[0].prefix.as_deref(), Some("dependency"));
        assert_eq!(
            metadata.plugins[0].artifact_id.as_deref(),
            Some("maven-dependency-plugin")
        );
    }

    #[test]
    fn empty_metadata_is_harmless() {
        let metadata = parse_metadata("<metadata/>").unwrap();
        assert!(metadata.versions().is_empty());
        assert_eq!(metadata.release(), None);
        assert_eq!(metadata.snapshot_version("jar", ""), None);
    }

    #[test]
    fn wrong_root_is_rejected() {
        assert!(parse_metadata("<project/>").is_err());
    }
}
