//! Loading the project in front of you.
//!
//! `jv tree` run in a directory has to answer the same question `mvn` does: which
//! POM is this, and what does it declare after inheritance? That means finding
//! `pom.xml`, building its effective model against the repository, and — for a
//! multi-module build — reading the sibling modules too, so that a dependency on
//! a sibling resolves from the working tree rather than from a repository that
//! has never heard of it.

use std::path::{Path, PathBuf};

use jv_model::{Artifact, Model, parse_pom};
use jv_model_builder::{ModelBuilder, SourcedModel};
use jv_resolver::CollectRequest;

use crate::error::DriverError;
use crate::source::RepositorySource;

/// A project loaded from disk.
#[derive(Clone, Debug)]
pub struct Project {
    /// The effective model.
    pub model: Model,
    /// The `pom.xml` it was read from.
    pub path: PathBuf,
    /// The modules found beneath it, effective models too. Empty for a
    /// single-module project.
    pub modules: Vec<Project>,
}

impl Project {
    /// The project's own coordinates.
    ///
    /// The extension is the packaging, which is what `dependency:tree` prints on
    /// the root line — a `pom`-packaged aggregator shows as `pom`, not `jar`.
    pub fn artifact(&self) -> Artifact {
        Artifact {
            group_id: self.model.group_id.clone().unwrap_or_default(),
            artifact_id: self.model.artifact_id.clone().unwrap_or_default(),
            version: self.model.version.clone().unwrap_or_default(),
            classifier: String::new(),
            extension: self.model.packaging_or_default().to_owned(),
        }
    }

    /// What to collect for this project: its own declared dependencies, under
    /// its own management.
    ///
    /// The root goes in as an *artifact* rather than as a dependency, which is
    /// what keeps the project's own `test` dependencies in the graph while a
    /// dependency's test dependencies stay out.
    pub fn collect_request(&self) -> CollectRequest {
        CollectRequest {
            root_artifact: Some(self.artifact()),
            root_dependency: None,
            dependencies: self.model.dependencies.clone(),
            managed_dependencies: self.model.dependency_management.clone(),
        }
    }

    /// Every project in the reactor, this one first, in declaration order.
    pub fn reactor(&self) -> Vec<&Project> {
        let mut all = vec![self];
        for module in &self.modules {
            all.extend(module.reactor());
        }
        all
    }
}

/// Finds the `pom.xml` governing a directory, walking upward.
///
/// Walking up is what makes `jv tree` work from inside `src/main/java`, which is
/// where people actually are when they want it.
pub fn find_pom(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(directory) = current {
        let candidate = directory.join("pom.xml");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = directory.parent();
    }
    None
}

/// Loads a project and, recursively, its modules.
///
/// Each POM is registered with `source` as it is read, so a module that inherits
/// from the aggregator — or depends on a sibling — resolves without a repository.
pub fn load_project(source: &RepositorySource, pom: &Path) -> Result<Project, DriverError> {
    load_at(source, pom, &mut Vec::new())
}

fn load_at(
    source: &RepositorySource,
    pom: &Path,
    visited: &mut Vec<PathBuf>,
) -> Result<Project, DriverError> {
    let canonical = pom.canonicalize().unwrap_or_else(|_| pom.to_path_buf());
    // A `<module>` pointing back at an ancestor would otherwise recurse forever.
    if visited.contains(&canonical) {
        return Err(DriverError::Other(format!(
            "{} appears more than once in the module tree",
            pom.display()
        )));
    }
    visited.push(canonical);

    let text = std::fs::read_to_string(pom).map_err(|source| DriverError::Io {
        path: pom.to_path_buf(),
        source,
    })?;
    let parsed = parse_pom(&text).map_err(|error| DriverError::Pom {
        source_name: pom.display().to_string(),
        source: error,
    })?;

    // Registered before building, and from the *raw* POM, because that is what a
    // parent lookup needs. The coordinates come from what the POM states plus one
    // hop into `<parent>`, which is as far as a raw POM can be trusted — and
    // which covers the shape every real multi-module build uses.
    if let (Some(group_id), Some(artifact_id), Some(version)) = (
        parsed.model.declared_or_parent_group_id(),
        parsed.model.artifact_id.as_deref(),
        parsed.model.declared_or_parent_version(),
    ) {
        source.register_reactor_pom(group_id, artifact_id, version, text.clone());
    }

    let directory = pom.parent().unwrap_or(Path::new("."));
    let sourced =
        SourcedModel::new(parsed.model, pom.display().to_string()).with_basedir(directory);

    let built = ModelBuilder::new(source, source.context().clone())
        .with_settings_profiles(source.settings_profiles())
        .with_lifecycle_bindings(source.lifecycle_bindings())
        .build(sourced)
        .map_err(|error| DriverError::Model {
            source_name: pom.display().to_string(),
            source: error,
        })?;

    for problem in built.errors() {
        source.record_warning(format!("{} ({})", problem.message, problem.source));
    }
    source.register_project_repositories(&built.model);

    let mut modules = Vec::new();
    for module in &built.model.modules {
        // `<module>` names a directory, but Maven also accepts a POM file.
        let as_directory = directory.join(module).join("pom.xml");
        let module_pom = if as_directory.is_file() {
            as_directory
        } else {
            directory.join(module)
        };
        if !module_pom.is_file() {
            source.record_warning(format!(
                "{}: module {module} has no pom.xml and was skipped",
                pom.display()
            ));
            continue;
        }
        modules.push(load_at(source, &module_pom, visited)?);
    }

    Ok(Project {
        model: built.model,
        path: pom.to_path_buf(),
        modules,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pom_is_found_from_a_nested_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        let nested = dir.path().join("src").join("main").join("java");
        std::fs::create_dir_all(&nested).unwrap();
        // People run `jv tree` from wherever they happen to be.
        assert_eq!(find_pom(&nested), Some(dir.path().join("pom.xml")));
    }

    #[test]
    fn the_nearest_pom_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        let module = dir.path().join("module");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join("pom.xml"), "<project/>").unwrap();
        assert_eq!(find_pom(&module), Some(module.join("pom.xml")));
    }

    #[test]
    fn a_directory_with_no_pom_above_it_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // A temp dir has no pom.xml anywhere up to the root on a normal machine.
        assert_eq!(find_pom(dir.path()), None);
    }
}
