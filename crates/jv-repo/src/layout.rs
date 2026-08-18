//! Where things live in a Maven 2 repository.
//!
//! Port of `Maven2RepositoryLayoutFactory`'s `getLocation`. The same layout
//! serves a remote repository, `~/.m2/repository`, and jv's own cache, so this
//! is the one place that turns coordinates into a path.
//!
//! The snapshot rule is the part worth stating twice: the **directory** uses the
//! base version and the **file name** uses the resolved version. A snapshot
//! deployed as `1.0-20240115.103000-7` therefore lives at
//! `…/1.0-SNAPSHOT/a-1.0-20240115.103000-7.jar`. Using one version for both
//! yields a path that looks reasonable and 404s.
//!
//! # This is a security boundary
//!
//! Coordinates come out of a POM, and a POM comes off the network from whoever
//! published a dependency of a dependency. Every function here treats them as
//! hostile input: a `<groupId>/tmp/anywhere</groupId>` would otherwise produce an
//! absolute path, and `Path::join` silently *discards its base* when given one,
//! so `jv sync` would write attacker-chosen bytes to an attacker-chosen
//! location. `..` in any field does the same thing more slowly.
//!
//! Maven's own `ModelValidator` restricts these fields to `[A-Za-z0-9._\-]+`, so
//! anything outside that set cannot address a real artifact and is replaced
//! rather than passed through. Sanitizing here rather than only validating at the
//! edges means no future caller can reintroduce the hole by building an
//! `Artifact` some other way.

use jv_model::Artifact;

/// The file name Maven keeps repository metadata under.
pub const METADATA_FILE: &str = "maven-metadata.xml";

/// The repository path of an artifact, relative to the repository root.
///
/// # Examples
///
/// ```
/// use jv_model::Artifact;
/// use jv_repo::artifact_path;
///
/// let jar = Artifact::new("org.slf4j", "slf4j-api", "2.0.9");
/// assert_eq!(
///     artifact_path(&jar),
///     "org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar"
/// );
///
/// // A resolved snapshot: the directory keeps the base version.
/// let snapshot = Artifact::new("g", "a", "1.0-20240115.103000-7");
/// assert_eq!(
///     artifact_path(&snapshot),
///     "g/a/1.0-SNAPSHOT/a-1.0-20240115.103000-7.jar"
/// );
/// ```
pub fn artifact_path(artifact: &Artifact) -> String {
    let mut path = String::with_capacity(128);
    push_group(&mut path, &artifact.group_id);
    push_segment(&mut path, &artifact.artifact_id);
    // The directory is keyed by the base version even when the file is not.
    push_segment(&mut path, &artifact.base_version());
    path.push_str(&safe_segment(&artifact.file_name()));
    path
}

/// Replaces anything in a path segment that could address something other than
/// the artifact it names.
///
/// `..` becomes `__`, and every character outside what Maven permits in an id
/// becomes `_`. A legitimate coordinate passes through unchanged, so this costs
/// nothing in the normal case and is the reason a hostile one cannot escape the
/// repository root.
fn safe_segment(segment: &str) -> String {
    if segment == ".." || segment == "." {
        // Not "" — an empty segment would silently collapse the path and put the
        // file one directory up, which is the thing being prevented.
        return "_".repeat(segment.len());
    }
    segment
        .chars()
        .map(|character| match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' | '+' => character,
            _ => '_',
        })
        .collect()
}

/// Which metadata file is being addressed.
///
/// The same name serves three purposes at three levels of the tree, and they
/// answer different questions: which versions of an artifact exist, which
/// timestamped build a snapshot currently resolves to, and which plugin prefixes
/// a group defines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataLocation<'a> {
    /// `group/maven-metadata.xml` — plugin prefix mappings.
    Group { group_id: &'a str },
    /// `group/artifact/maven-metadata.xml` — the released versions.
    Artifact {
        group_id: &'a str,
        artifact_id: &'a str,
    },
    /// `group/artifact/version/maven-metadata.xml` — a snapshot's current build.
    Version {
        group_id: &'a str,
        artifact_id: &'a str,
        version: &'a str,
    },
}

impl MetadataLocation<'_> {
    /// The repository path of this metadata file.
    ///
    /// # Examples
    ///
    /// ```
    /// use jv_repo::MetadataLocation;
    ///
    /// assert_eq!(
    ///     MetadataLocation::Artifact { group_id: "org.slf4j", artifact_id: "slf4j-api" }.path(),
    ///     "org/slf4j/slf4j-api/maven-metadata.xml"
    /// );
    /// assert_eq!(
    ///     MetadataLocation::Version {
    ///         group_id: "g", artifact_id: "a", version: "1.0-SNAPSHOT",
    ///     }.path(),
    ///     "g/a/1.0-SNAPSHOT/maven-metadata.xml"
    /// );
    /// ```
    pub fn path(&self) -> String {
        let mut path = String::with_capacity(96);
        match self {
            MetadataLocation::Group { group_id } => {
                push_group(&mut path, group_id);
            }
            MetadataLocation::Artifact {
                group_id,
                artifact_id,
            } => {
                push_group(&mut path, group_id);
                push_segment(&mut path, artifact_id);
            }
            MetadataLocation::Version {
                group_id,
                artifact_id,
                version,
            } => {
                push_group(&mut path, group_id);
                push_segment(&mut path, artifact_id);
                push_segment(&mut path, version);
            }
        }
        path.push_str(METADATA_FILE);
        path
    }
}

/// Appends a group id as directories, skipping an empty one so that group-less
/// metadata lands at the repository root the way upstream puts it.
fn push_group(path: &mut String, group_id: &str) {
    if group_id.is_empty() {
        return;
    }
    // A group id becomes several segments, so each one is sanitized separately.
    // Sanitizing the joined string instead would turn the separators into `_`.
    for part in group_id.split('.') {
        if part.is_empty() {
            continue;
        }
        path.push_str(&safe_segment(part));
        path.push('/');
    }
}

fn push_segment(path: &mut String, segment: &str) {
    if segment.is_empty() {
        return;
    }
    path.push_str(&safe_segment(segment));
    path.push('/');
}

/// A checksum algorithm a repository publishes alongside its files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Checksum {
    /// Always published, and the one Maven verifies by default.
    Sha1,
    /// Published by older repositories; weak, and only worth checking when
    /// nothing better is present.
    Md5,
    /// Published by newer repositories.
    Sha256,
    Sha512,
}

impl Checksum {
    /// The extension appended to the file's own path.
    pub fn extension(self) -> &'static str {
        match self {
            Checksum::Sha1 => "sha1",
            Checksum::Md5 => "md5",
            Checksum::Sha256 => "sha256",
            Checksum::Sha512 => "sha512",
        }
    }

    /// The number of hex characters a valid digest has.
    pub fn hex_len(self) -> usize {
        match self {
            Checksum::Md5 => 32,
            Checksum::Sha1 => 40,
            Checksum::Sha256 => 64,
            Checksum::Sha512 => 128,
        }
    }
}

/// The path of a file's checksum.
///
/// # Examples
///
/// ```
/// use jv_repo::{Checksum, checksum_path};
///
/// assert_eq!(
///     checksum_path("g/a/1.0/a-1.0.jar", Checksum::Sha1),
///     "g/a/1.0/a-1.0.jar.sha1"
/// );
/// ```
pub fn checksum_path(path: &str, checksum: Checksum) -> String {
    format!("{path}.{}", checksum.extension())
}

/// Joins a repository base URL to a repository-relative path.
///
/// Repository URLs are written both with and without a trailing slash, and
/// concatenating the two naively yields either a doubled or a missing separator.
///
/// # Examples
///
/// ```
/// use jv_repo::join_url;
///
/// assert_eq!(
///     join_url("https://repo.maven.apache.org/maven2", "g/a/1.0/a-1.0.jar"),
///     "https://repo.maven.apache.org/maven2/g/a/1.0/a-1.0.jar"
/// );
/// assert_eq!(
///     join_url("https://repo.maven.apache.org/maven2/", "g/a/1.0/a-1.0.jar"),
///     "https://repo.maven.apache.org/maven2/g/a/1.0/a-1.0.jar"
/// );
/// ```
pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every path this crate produces has to stay under the root it is joined
    /// to, whatever a POM says. These are the shapes that got through before.
    #[test]
    fn a_hostile_coordinate_cannot_escape_the_repository_root() {
        let root = std::path::Path::new("/repo");
        for (group_id, artifact_id, version, classifier, extension) in [
            // An absolute group id is the worst of them: `Path::join` discards
            // its base when given an absolute path, so this wrote wherever it
            // liked rather than one directory up.
            ("/tmp/anywhere", "evil", "1.0", "", "jar"),
            ("..", "..", "..", "", "jar"),
            ("com.example", "..", "1.0", "", "jar"),
            ("com.example", "a", "../..", "", "jar"),
            ("com.example", "a", "1.0", "../../../../etc/x", "jar"),
            ("com.example", "a", "1.0", "", "../../../../etc/passwd"),
            ("c:\\windows", "a", "1.0", "", "jar"),
            ("com/example", "a", "1.0", "", "jar"),
            ("com.example", "a\u{0}", "1.0", "", "jar"),
        ] {
            let artifact = Artifact {
                group_id: group_id.to_owned(),
                artifact_id: artifact_id.to_owned(),
                version: version.to_owned(),
                classifier: classifier.to_owned(),
                extension: extension.to_owned(),
            };
            let relative = artifact_path(&artifact);
            assert!(
                !relative.split('/').any(|segment| segment == ".."),
                "{relative} still contains a traversal segment"
            );
            assert!(
                root.join(&relative).starts_with(root),
                "{relative} escaped the root"
            );
        }
    }

    #[test]
    fn an_ordinary_coordinate_is_untouched() {
        // Sanitizing must cost nothing in the normal case, or every path in
        // every repository changes and the cache is invalidated for no reason.
        let artifact = Artifact::new(
            "org.springframework.boot",
            "spring-boot-starter-web",
            "3.3.0",
        );
        assert_eq!(
            artifact_path(&artifact),
            "org/springframework/boot/spring-boot-starter-web/3.3.0/spring-boot-starter-web-3.3.0.jar"
        );
        // Including the characters real coordinates do use.
        let odd = Artifact {
            group_id: "com.example_x".to_owned(),
            artifact_id: "a-b.c".to_owned(),
            version: "1.0.0-RC1+build.5".to_owned(),
            classifier: "natives-linux".to_owned(),
            extension: "tar.gz".to_owned(),
        };
        assert_eq!(
            artifact_path(&odd),
            "com/example_x/a-b.c/1.0.0-RC1+build.5/a-b.c-1.0.0-RC1+build.5-natives-linux.tar.gz"
        );
    }

    #[test]
    fn metadata_paths_are_sanitized_too() {
        let path = MetadataLocation::Artifact {
            group_id: "../../etc",
            artifact_id: "..",
        }
        .path();
        assert!(!path.split('/').any(|segment| segment == ".."));
        assert!(
            std::path::Path::new("/repo")
                .join(&path)
                .starts_with("/repo")
        );
    }

    #[test]
    fn group_ids_become_directories() {
        let artifact = Artifact::new("com.fasterxml.jackson.core", "jackson-databind", "2.17.0");
        assert_eq!(
            artifact_path(&artifact),
            "com/fasterxml/jackson/core/jackson-databind/2.17.0/jackson-databind-2.17.0.jar"
        );
    }

    #[test]
    fn classifier_and_extension_shape_the_file_name() {
        let sources = Artifact::new("g", "a", "1.0").with_classifier("sources");
        assert_eq!(artifact_path(&sources), "g/a/1.0/a-1.0-sources.jar");

        let pom = Artifact::new("g", "a", "1.0").with_extension("pom");
        assert_eq!(artifact_path(&pom), "g/a/1.0/a-1.0.pom");

        let war = Artifact::new("g", "a", "1.0").with_extension("war");
        assert_eq!(artifact_path(&war), "g/a/1.0/a-1.0.war");
    }

    #[test]
    fn a_declared_snapshot_uses_one_version_throughout() {
        let artifact = Artifact::new("g", "a", "1.0-SNAPSHOT");
        assert_eq!(
            artifact_path(&artifact),
            "g/a/1.0-SNAPSHOT/a-1.0-SNAPSHOT.jar"
        );
    }

    #[test]
    fn a_resolved_snapshot_splits_directory_from_file_name() {
        // The trap: the directory is the base version, the file is not.
        let artifact = Artifact::new("g", "a", "1.0-20240115.103000-7");
        assert_eq!(
            artifact_path(&artifact),
            "g/a/1.0-SNAPSHOT/a-1.0-20240115.103000-7.jar"
        );
        let with_classifier = artifact.with_classifier("sources");
        assert_eq!(
            artifact_path(&with_classifier),
            "g/a/1.0-SNAPSHOT/a-1.0-20240115.103000-7-sources.jar"
        );
    }

    #[test]
    fn metadata_sits_at_three_levels() {
        assert_eq!(
            MetadataLocation::Group {
                group_id: "org.apache.maven.plugins"
            }
            .path(),
            "org/apache/maven/plugins/maven-metadata.xml"
        );
        assert_eq!(
            MetadataLocation::Artifact {
                group_id: "org.slf4j",
                artifact_id: "slf4j-api",
            }
            .path(),
            "org/slf4j/slf4j-api/maven-metadata.xml"
        );
        assert_eq!(
            MetadataLocation::Version {
                group_id: "org.slf4j",
                artifact_id: "slf4j-api",
                version: "2.0.9-SNAPSHOT",
            }
            .path(),
            "org/slf4j/slf4j-api/2.0.9-SNAPSHOT/maven-metadata.xml"
        );
    }

    #[test]
    fn metadata_without_a_group_lands_at_the_root() {
        assert_eq!(
            MetadataLocation::Group { group_id: "" }.path(),
            "maven-metadata.xml"
        );
    }

    #[test]
    fn checksum_paths_and_lengths() {
        assert_eq!(
            checksum_path("g/a/1.0/a-1.0.jar", Checksum::Sha1),
            "g/a/1.0/a-1.0.jar.sha1"
        );
        assert_eq!(
            checksum_path("g/a/maven-metadata.xml", Checksum::Md5),
            "g/a/maven-metadata.xml.md5"
        );
        assert_eq!(Checksum::Sha1.hex_len(), 40);
        assert_eq!(Checksum::Md5.hex_len(), 32);
        assert_eq!(Checksum::Sha256.hex_len(), 64);
        assert_eq!(Checksum::Sha512.hex_len(), 128);
    }

    #[test]
    fn url_joining_tolerates_either_spelling() {
        for base in [
            "https://repo1.maven.org/maven2",
            "https://repo1.maven.org/maven2/",
        ] {
            assert_eq!(
                join_url(base, "g/a/1.0/a-1.0.jar"),
                "https://repo1.maven.org/maven2/g/a/1.0/a-1.0.jar"
            );
        }
        // A leading slash on the path is not a second separator.
        assert_eq!(
            join_url("https://host/base/", "/g/a"),
            "https://host/base/g/a"
        );
    }

    #[test]
    fn a_file_url_joins_the_same_way() {
        assert_eq!(
            join_url("file:///home/me/.m2/repository", "g/a/1.0/a-1.0.jar"),
            "file:///home/me/.m2/repository/g/a/1.0/a-1.0.jar"
        );
    }
}
