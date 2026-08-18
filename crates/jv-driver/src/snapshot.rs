//! Making a synced snapshot resolvable offline.
//!
//! A release needs nothing but its file: Maven finds it by path. A snapshot does
//! not, because the file name in a repository carries a deployment timestamp —
//! `a-1.0-20240115.103000-7.jar` — and Maven only learns which timestamp is
//! current by reading metadata beside it.
//!
//! # Why not simply copy the remote layout
//!
//! The obvious move is to place the timestamped file and write the
//! `maven-metadata-<repositoryId>.xml` that a download would have produced. It
//! does not work in general: the id in that file name is the *effective*
//! repository id, which is the mirror's when the user has a mirror, and jv
//! cannot know what the next `mvn` invocation will be configured with. Guess
//! wrong and Maven ignores the metadata, which is worse than not writing it —
//! the artifact is present and still unresolvable.
//!
//! # What jv writes instead
//!
//! The layout `mvn install` produces, which has no repository id in it at all:
//! the file under its **base** `-SNAPSHOT` name, and a `maven-metadata-local.xml`
//! declaring `<localCopy>true</localCopy>`. Maven accepts that from any
//! configuration, because it is the shape it writes itself.
//!
//! That is also honest about what happened: jv put the file there, so it *is*
//! locally installed, whatever it was downloaded from. Confirmed by running
//! `mvn install` on a snapshot project and reading what landed in `~/.m2`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use jv_model::Artifact;

use crate::error::DriverError;

/// The file name Maven reads a locally installed snapshot's versions from.
pub const LOCAL_METADATA: &str = "maven-metadata-local.xml";

/// The `(extension, classifier)` pairs placed for one snapshot version.
#[derive(Debug, Default)]
pub struct LocalSnapshot {
    group_id: String,
    artifact_id: String,
    /// The `-SNAPSHOT` version, not the timestamped one.
    base_version: String,
    /// Sorted so the file is byte-stable across runs.
    files: BTreeSet<(String, String)>,
}

impl LocalSnapshot {
    pub fn new(artifact: &Artifact) -> Self {
        Self {
            group_id: artifact.group_id.clone(),
            artifact_id: artifact.artifact_id.clone(),
            base_version: artifact.base_version(),
            files: BTreeSet::new(),
        }
    }

    /// Records that a file of this extension and classifier was placed.
    pub fn record(&mut self, artifact: &Artifact) {
        self.files
            .insert((artifact.extension.clone(), artifact.classifier.clone()));
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Writes `maven-metadata-local.xml` into a version directory.
    pub fn write(&self, directory: &Path) -> Result<PathBuf, DriverError> {
        let stamp = stamp(SystemTime::now());
        let mut xml = String::with_capacity(512);
        xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        xml.push_str("<metadata modelVersion=\"1.1.0\">\n");
        xml.push_str(&format!("  <groupId>{}</groupId>\n", self.group_id));
        xml.push_str(&format!(
            "  <artifactId>{}</artifactId>\n",
            self.artifact_id
        ));
        xml.push_str("  <versioning>\n");
        xml.push_str(&format!("    <lastUpdated>{stamp}</lastUpdated>\n"));
        xml.push_str("    <snapshot>\n      <localCopy>true</localCopy>\n    </snapshot>\n");
        xml.push_str("    <snapshotVersions>\n");
        for (extension, classifier) in &self.files {
            xml.push_str("      <snapshotVersion>\n");
            if !classifier.is_empty() {
                xml.push_str(&format!("        <classifier>{classifier}</classifier>\n"));
            }
            xml.push_str(&format!("        <extension>{extension}</extension>\n"));
            // The value is the base version, because that is the name the file
            // was placed under.
            xml.push_str(&format!("        <value>{}</value>\n", self.base_version));
            xml.push_str(&format!("        <updated>{stamp}</updated>\n"));
            xml.push_str("      </snapshotVersion>\n");
        }
        xml.push_str("    </snapshotVersions>\n  </versioning>\n");
        xml.push_str(&format!("  <version>{}</version>\n", self.base_version));
        xml.push_str("</metadata>\n");

        let path = directory.join(LOCAL_METADATA);
        std::fs::write(&path, xml).map_err(|source| DriverError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }
}

/// `yyyyMMddHHmmss` in UTC, the format Maven stamps metadata with.
///
/// Computed rather than formatted by a date library: the only question is which
/// civil date a count of seconds falls on, and pulling in a calendar crate to
/// answer it would be the larger cost.
fn stamp(at: SystemTime) -> String {
    let seconds = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Howard Hinnant's `civil_from_days`, which is the standard closed form for
/// this and avoids a table of month lengths and leap-year special cases.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_stamp_is_mavens_format() {
        // Epoch, and a date checked against `date -u`.
        assert_eq!(stamp(UNIX_EPOCH), "19700101000000");
        assert_eq!(
            stamp(UNIX_EPOCH + Duration::from_secs(1_705_314_600)),
            "20240115103000"
        );
        // A leap day, which is what the closed form is for.
        assert_eq!(
            stamp(UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
            "20240229000000"
        );
        assert_eq!(stamp(UNIX_EPOCH).len(), 14);
    }

    #[test]
    fn the_metadata_declares_a_local_copy_at_the_base_version() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = Artifact {
            group_id: "com.example".to_owned(),
            artifact_id: "lib".to_owned(),
            // What a repository served, timestamped.
            version: "1.0-20240115.103000-7".to_owned(),
            classifier: String::new(),
            extension: "jar".to_owned(),
        };
        let mut snapshot = LocalSnapshot::new(&artifact);
        snapshot.record(&artifact);
        snapshot.record(&Artifact {
            extension: "pom".to_owned(),
            ..artifact.clone()
        });
        let written = std::fs::read_to_string(snapshot.write(dir.path()).unwrap()).unwrap();

        // `localCopy` is what makes Maven accept this from any configuration —
        // there is no repository id anywhere in the file to get wrong.
        assert!(written.contains("<localCopy>true</localCopy>"));
        assert!(written.contains("<version>1.0-SNAPSHOT</version>"));
        // Each value is the base version, because that is the name the file was
        // placed under.
        assert_eq!(written.matches("<value>1.0-SNAPSHOT</value>").count(), 2);
        assert!(written.contains("<extension>jar</extension>"));
        assert!(written.contains("<extension>pom</extension>"));
        assert!(!written.contains("20240115.103000-7"));
    }

    #[test]
    fn a_classifier_is_recorded_and_a_bare_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = Artifact::new("g", "a", "1.0-SNAPSHOT");
        let mut snapshot = LocalSnapshot::new(&artifact);
        snapshot.record(&artifact);
        snapshot.record(&artifact.clone().with_classifier("sources"));
        let written = std::fs::read_to_string(snapshot.write(dir.path()).unwrap()).unwrap();
        assert_eq!(written.matches("<snapshotVersion>").count(), 2);
        assert_eq!(written.matches("<classifier>").count(), 1);
        assert!(written.contains("<classifier>sources</classifier>"));
    }
}
