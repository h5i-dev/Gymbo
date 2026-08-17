//! Where POMs come from.
//!
//! Model building is deliberately **synchronous** here, unlike dependency
//! collection. A parent chain is serial by nature — you cannot know which POM to
//! fetch next until you have read the current one — so the pure-core/effectful-
//! driver split that makes dependency resolution parallelizable would buy nothing
//! and cost clarity. The concurrency that matters lives in `jv-resolver`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jv_model::Model;

/// A POM together with where it came from.
#[derive(Clone, Debug)]
pub struct SourcedModel {
    pub model: Model,
    /// A coordinate or path, used in problem reports.
    pub source: String,
    /// The directory holding the POM, when it is a file. Needed for
    /// `<relativePath>` parents, `${project.basedir}` and file-based profile
    /// activation; absent for a POM read out of a repository.
    pub basedir: Option<PathBuf>,
}

impl SourcedModel {
    pub fn new(model: Model, source: impl Into<String>) -> Self {
        Self {
            model,
            source: source.into(),
            basedir: None,
        }
    }

    pub fn with_basedir(mut self, basedir: impl Into<PathBuf>) -> Self {
        self.basedir = Some(basedir.into());
        self
    }

    /// `groupId:artifactId:version` as the POM states it, for cycle detection and
    /// messages.
    pub fn coordinates(&self) -> String {
        self.model.coordinates_hint()
    }
}

/// Supplies POMs by coordinate or by path.
pub trait ModelSource {
    /// Reads the POM for these coordinates, from a reactor or a repository.
    fn get(&self, group_id: &str, artifact_id: &str, version: &str)
    -> Result<SourcedModel, String>;

    /// Reads a POM from disk, returning `None` when there is none there.
    ///
    /// Used for `<relativePath>` parents. The default implementation reports
    /// nothing on disk, which makes a repository-only source valid.
    fn get_at_path(&self, _path: &Path) -> Result<Option<SourcedModel>, String> {
        Ok(None)
    }
}

/// An in-memory source, for tests and for resolving against a fixed set of POMs.
#[derive(Debug, Default)]
pub struct MapModelSource {
    by_coordinates: HashMap<String, String>,
    by_path: HashMap<PathBuf, String>,
}

impl MapModelSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a POM under its coordinates.
    pub fn insert(
        &mut self,
        group_id: &str,
        artifact_id: &str,
        version: &str,
        pom: impl Into<String>,
    ) -> &mut Self {
        self.by_coordinates
            .insert(format!("{group_id}:{artifact_id}:{version}"), pom.into());
        self
    }

    /// Registers a POM at a path, so `<relativePath>` resolution can find it.
    pub fn insert_at_path(
        &mut self,
        path: impl Into<PathBuf>,
        pom: impl Into<String>,
    ) -> &mut Self {
        self.by_path.insert(path.into(), pom.into());
        self
    }
}

impl ModelSource for MapModelSource {
    fn get(
        &self,
        group_id: &str,
        artifact_id: &str,
        version: &str,
    ) -> Result<SourcedModel, String> {
        let key = format!("{group_id}:{artifact_id}:{version}");
        let xml = self
            .by_coordinates
            .get(&key)
            .ok_or_else(|| format!("no POM registered for {key}"))?;
        let parsed = jv_model::parse_pom(xml).map_err(|error| error.to_string())?;
        Ok(SourcedModel::new(parsed.model, key))
    }

    fn get_at_path(&self, path: &Path) -> Result<Option<SourcedModel>, String> {
        // A directory reference means the POM inside it.
        let candidates = [path.to_path_buf(), path.join("pom.xml")];
        for candidate in candidates {
            if let Some(xml) = self.by_path.get(&candidate) {
                let parsed = jv_model::parse_pom(xml).map_err(|error| error.to_string())?;
                let basedir = candidate
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                return Ok(Some(
                    SourcedModel::new(parsed.model, candidate.display().to_string())
                        .with_basedir(basedir),
                ));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_source_resolves_by_coordinates() {
        let mut source = MapModelSource::new();
        source.insert(
            "g",
            "a",
            "1.0",
            r#"<project><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></project>"#,
        );
        let found = source.get("g", "a", "1.0").expect("found");
        assert_eq!(found.model.artifact_id.as_deref(), Some("a"));
        assert_eq!(found.source, "g:a:1.0");
        assert!(source.get("g", "a", "2.0").is_err());
    }

    #[test]
    fn map_source_resolves_a_directory_to_its_pom() {
        let mut source = MapModelSource::new();
        source.insert_at_path(
            "/work/parent/pom.xml",
            r#"<project><artifactId>parent</artifactId></project>"#,
        );
        let by_file = source
            .get_at_path(Path::new("/work/parent/pom.xml"))
            .expect("ok");
        assert!(by_file.is_some());
        let by_dir = source.get_at_path(Path::new("/work/parent")).expect("ok");
        assert_eq!(
            by_dir.expect("found").basedir.as_deref(),
            Some(Path::new("/work/parent"))
        );
        assert!(
            source
                .get_at_path(Path::new("/nope"))
                .expect("ok")
                .is_none()
        );
    }
}
