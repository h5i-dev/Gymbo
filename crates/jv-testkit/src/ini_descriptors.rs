//! Maven Resolver's `.ini` artifact descriptors.
//!
//! Port of `IniArtifactDataReader` and `IniArtifactDescriptorReader`. These
//! files stand in for POMs in the collection tests: instead of resolving real
//! artifacts, the collector asks for `gid_aid_version.ini` and gets back a
//! dependency list. Reading them is what makes the 600-odd files under
//! `maven-resolver-impl/src/test/resources/artifact-descriptions/` usable.
//!
//! ```text
//! [relocation]
//! gid:aid:ext:ver
//!
//! [dependencies]
//! gid:aid:ext:ver:scope
//! -excluded-gid:excluded-aid
//!
//! [managed-dependencies]
//! gid:aid:ext:ver:compile
//!
//! [repositories]
//! id:type:http://example.com/repo
//! ```
//!
//! Two details differ from every other Maven format and will silently produce
//! the wrong graph if missed. The third coordinate field is the **extension**,
//! not the version — `gid:aid:ext:ver` — and there is no classifier field at
//! all. And the two dependency sections have **different defaults**: an omitted
//! scope means `compile` under `[dependencies]` but *unset* under
//! `[managed-dependencies]`, because a managed entry that pinned a scope would
//! manage one.

use std::path::PathBuf;

use jv_model::{Artifact, Dependency, Exclusion, Scope};

/// A descriptor file that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum IniError {
    #[error("line {line}: content before any [section] header: {text}")]
    NoSection { line: usize, text: String },
    #[error("line {line}: {text:?} needs at least groupId:artifactId:extension:version")]
    Coordinates { line: usize, text: String },
    #[error("line {line}: an exclusion must follow a dependency")]
    DanglingExclusion { line: usize },
    #[error("line {line}: {text:?} is not groupId:artifactId")]
    Exclusion { line: usize, text: String },
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A repository entry, kept for completeness; jv's graph tests do not use them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestRepository {
    pub id: String,
    pub type_: String,
    pub url: String,
}

/// What one `.ini` file says about an artifact.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtifactDescription {
    /// Where this artifact moved to. The reader follows it and discards
    /// everything else the relocating file said.
    pub relocation: Option<Artifact>,
    pub dependencies: Vec<Dependency>,
    pub managed_dependencies: Vec<Dependency>,
    pub repositories: Vec<TestRepository>,
    /// Scope strings the file used that jv does not model.
    ///
    /// Maven's `Dependency.scope` is a free string, while jv narrows it to an
    /// enum. Exactly one corpus file exploits that freedom, using a synthetic
    /// `managedScope` marker to prove that management was applied. Recording it
    /// keeps the difference visible rather than failing the read or silently
    /// pretending the scope was absent.
    pub unmodelled_scopes: Vec<String>,
}

/// Which section a line belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Relocation,
    Dependencies,
    ManagedDependencies,
    Repositories,
    /// A header the reader does not recognize; its lines are ignored.
    Other,
}

impl Section {
    /// Upstream normalizes a header by dropping the brackets, removing every
    /// `-`, and upper-casing, so `[managed-dependencies]`,
    /// `[managedDependencies]` and `[manageddependencies]` are one section.
    fn parse(header: &str) -> Self {
        let normalized: String = header
            .chars()
            .filter(|c| *c != '-')
            .flat_map(char::to_uppercase)
            .collect();
        match normalized.as_str() {
            "RELOCATION" => Section::Relocation,
            "DEPENDENCIES" => Section::Dependencies,
            "MANAGEDDEPENDENCIES" => Section::ManagedDependencies,
            "REPOSITORIES" => Section::Repositories,
            _ => Section::Other,
        }
    }
}

/// Parses one descriptor file.
pub fn parse_description(text: &str) -> Result<ArtifactDescription, IniError> {
    let mut description = ArtifactDescription::default();
    let mut section: Option<Section> = None;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let parsed = Section::parse(header);
            // Repeating a header starts that section over rather than appending.
            match parsed {
                Section::Relocation => description.relocation = None,
                Section::Dependencies => description.dependencies.clear(),
                Section::ManagedDependencies => description.managed_dependencies.clear(),
                Section::Repositories => description.repositories.clear(),
                Section::Other => {}
            }
            section = Some(parsed);
            continue;
        }

        let line_number = index + 1;
        let Some(section) = section else {
            return Err(IniError::NoSection {
                line: line_number,
                text: line.to_owned(),
            });
        };

        match section {
            Section::Other => {}
            Section::Relocation => {
                description.relocation = Some(parse_artifact(line_number, line)?);
            }
            Section::Repositories => {
                // Only the first two colons separate; the URL keeps its own.
                let mut parts = line.splitn(3, ':');
                description.repositories.push(TestRepository {
                    id: parts.next().unwrap_or_default().to_owned(),
                    type_: parts.next().unwrap_or_default().to_owned(),
                    url: parts.next().unwrap_or_default().to_owned(),
                });
            }
            Section::Dependencies | Section::ManagedDependencies => {
                let managed = section == Section::ManagedDependencies;
                if let Some(excluded) = line.strip_prefix('-') {
                    let list = if managed {
                        &mut description.managed_dependencies
                    } else {
                        &mut description.dependencies
                    };
                    let dependency = list
                        .last_mut()
                        .ok_or(IniError::DanglingExclusion { line: line_number })?;
                    dependency
                        .exclusions
                        .push(parse_exclusion(line_number, excluded)?);
                } else {
                    let (dependency, unmodelled) = parse_dependency(line_number, line, managed)?;
                    if let Some(scope) = unmodelled {
                        description.unmodelled_scopes.push(scope);
                    }
                    let list = if managed {
                        &mut description.managed_dependencies
                    } else {
                        &mut description.dependencies
                    };
                    list.push(dependency);
                }
            }
        }
    }

    Ok(description)
}

/// Parses `groupId:artifactId:extension:version`.
///
/// Note the third field: these files carry an extension where every other Maven
/// format carries a version, and no classifier field exists at all.
fn parse_artifact(line: usize, text: &str) -> Result<Artifact, IniError> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 4 {
        return Err(IniError::Coordinates {
            line,
            text: text.to_owned(),
        });
    }
    Ok(Artifact {
        group_id: parts[0].to_owned(),
        artifact_id: parts[1].to_owned(),
        extension: parts[2].to_owned(),
        version: parts[3].to_owned(),
        classifier: String::new(),
    })
}

/// Parses a dependency line, whose defaults depend on which section it is in.
///
/// Returns the scope string alongside when it is one jv does not model, so the
/// caller can report it rather than lose it.
fn parse_dependency(
    line: usize,
    text: &str,
    managed: bool,
) -> Result<(Dependency, Option<String>), IniError> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 4 {
        return Err(IniError::Coordinates {
            line,
            text: text.to_owned(),
        });
    }

    let scope_text = parts.get(4).copied().unwrap_or(if managed {
        // A managed entry with no scope manages no scope.
        ""
    } else {
        "compile"
    });
    let mut unmodelled = None;
    let scope = if scope_text.is_empty() {
        None
    } else {
        match scope_text.parse::<Scope>() {
            Ok(scope) => Some(scope),
            Err(_) => {
                unmodelled = Some(scope_text.to_owned());
                None
            }
        }
    };

    let optional = match parts.get(5).copied() {
        Some(flag) => Some(!flag.starts_with('!')),
        // Managed entries keep "unset" distinct from "explicitly not optional",
        // because management only fills in what a declaration left out.
        None if managed => None,
        None => Some(false),
    };

    Ok((
        Dependency {
            group_id: parts[0].to_owned(),
            artifact_id: parts[1].to_owned(),
            version: Some(parts[3].to_owned()),
            // The extension travels as the type, which is how these files spell
            // it.
            type_: Some(parts[2].to_owned()),
            classifier: None,
            scope,
            optional,
            exclusions: Vec::new(),
            system_path: None,
        },
        unmodelled,
    ))
}

fn parse_exclusion(line: usize, text: &str) -> Result<Exclusion, IniError> {
    let (group_id, artifact_id) = text.split_once(':').ok_or_else(|| IniError::Exclusion {
        line,
        text: text.to_owned(),
    })?;
    Ok(Exclusion::new(group_id, artifact_id))
}

/// Serves descriptors out of a directory, the way the collection tests do.
#[derive(Clone, Debug)]
pub struct DescriptorReader {
    root: PathBuf,
}

impl DescriptorReader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The file an artifact's description lives in.
    ///
    /// The name carries only group, artifact and version — never the extension
    /// or classifier — and nothing is escaped, so `gid:b-alt:jar:1.0` becomes
    /// `gid_b-alt_1.0.ini`.
    pub fn path_of(&self, artifact: &Artifact) -> PathBuf {
        self.root.join(format!(
            "{}_{}_{}.ini",
            artifact.group_id, artifact.artifact_id, artifact.version
        ))
    }

    /// Reads a description, following a relocation to its target.
    ///
    /// A relocating file's other sections are discarded, matching upstream:
    /// the relocation replaces the artifact rather than adding to it.
    pub fn get(&self, artifact: &Artifact) -> Result<ArtifactDescription, IniError> {
        Ok(self.resolve(artifact)?.description)
    }

    /// Reads a description and reports where the relocations led.
    ///
    /// The chain is what a caller needs to build a faithful `Descriptor`:
    /// swallowing it — which `get` does, and which is all this offered before —
    /// leaves the collector unable to tell a relocated artifact from a plain
    /// one, so the corpus's two relocation goldens went unexercised.
    pub fn resolve(&self, artifact: &Artifact) -> Result<Resolved, IniError> {
        let mut current = artifact.clone();
        let mut relocations = Vec::new();
        // A relocation chain is short in practice; the bound only stops a
        // corpus file that points at itself from hanging the test.
        for _ in 0..16 {
            let path = self.path_of(&current);
            let text = std::fs::read_to_string(&path).map_err(|source| IniError::Io {
                path: path.clone(),
                source,
            })?;
            let description = parse_description(&text)?;
            match &description.relocation {
                Some(target) => {
                    relocations.push(current.clone());
                    current = target.clone();
                }
                None => {
                    return Ok(Resolved {
                        description,
                        artifact: current,
                        relocations,
                    });
                }
            }
        }
        Ok(Resolved {
            description: ArtifactDescription::default(),
            artifact: current,
            relocations,
        })
    }
}

impl DescriptorReader {
    /// The versions the corpus holds for an artifact, ascending.
    ///
    /// Read off the file names, since the corpus is one `.ini` per version and
    /// has no index. A range cannot expand without this, which is why the two
    /// range goldens sat unexercised.
    pub fn versions(&self, group_id: &str, artifact_id: &str) -> Vec<String> {
        let prefix = format!("{group_id}_{artifact_id}_");
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut versions: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| {
                let stem = name.strip_suffix(".ini")?;
                Some(stem.strip_prefix(&prefix)?.to_owned())
            })
            .collect();
        versions.sort_by(|left, right| {
            jv_version::Version::parse(left).cmp(&jv_version::Version::parse(right))
        });
        versions
    }
}

/// A description, with the coordinates it was actually found under.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub description: ArtifactDescription,
    /// Where the artifact really lives, after any relocations.
    pub artifact: Artifact,
    /// Coordinates it was relocated *from*, oldest first.
    pub relocations: Vec<Artifact>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn dependencies_default_to_compile_and_not_optional() {
        let description = parse_description("[dependencies]\ngid:aid:jar:1.0\n").unwrap();
        let dependency = &description.dependencies[0];
        assert_eq!(dependency.group_id, "gid");
        assert_eq!(dependency.artifact_id, "aid");
        // The third field is the extension, carried as the type.
        assert_eq!(dependency.type_.as_deref(), Some("jar"));
        assert_eq!(dependency.version.as_deref(), Some("1.0"));
        assert_eq!(dependency.scope, Some(Scope::Compile));
        assert_eq!(dependency.optional, Some(false));
    }

    #[test]
    fn managed_dependencies_leave_defaults_unset() {
        let description = parse_description("[managed-dependencies]\ngid:aid:jar:1.0\n").unwrap();
        let managed = &description.managed_dependencies[0];
        // Unset, not compile: management fills gaps rather than creating them.
        assert_eq!(managed.scope, None);
        assert_eq!(managed.optional, None);
    }

    #[test]
    fn scope_and_optional_are_read() {
        let description = parse_description(
            "[dependencies]\n\
             gid:a:jar:1.0:test\n\
             gid:b:jar:1.0:runtime:optional\n\
             gid:c:jar:1.0:provided:!optional\n",
        )
        .unwrap();
        assert_eq!(description.dependencies[0].scope, Some(Scope::Test));
        assert_eq!(description.dependencies[1].scope, Some(Scope::Runtime));
        assert_eq!(description.dependencies[1].optional, Some(true));
        assert_eq!(description.dependencies[2].optional, Some(false));
    }

    #[test]
    fn exclusions_attach_to_the_dependency_above() {
        let description = parse_description(
            "[dependencies]\n\
             gid:a:jar:1.0\n\
             -excluded:one\n\
             -excluded:two\n\
             gid:b:jar:1.0\n",
        )
        .unwrap();
        assert_eq!(description.dependencies[0].exclusions.len(), 2);
        assert!(description.dependencies[0].exclusions[0].matches("excluded", "one"));
        assert!(description.dependencies[1].exclusions.is_empty());
    }

    #[test]
    fn an_exclusion_without_a_dependency_is_an_error() {
        let error = parse_description("[dependencies]\n-a:b\n").unwrap_err();
        assert!(matches!(error, IniError::DanglingExclusion { .. }));
    }

    #[test]
    fn header_spellings_are_equivalent() {
        for header in [
            "[managed-dependencies]",
            "[managedDependencies]",
            "[manageddependencies]",
            "[MANAGED-DEPENDENCIES]",
        ] {
            let description = parse_description(&format!("{header}\ngid:aid:jar:1.0\n")).unwrap();
            assert_eq!(
                description.managed_dependencies.len(),
                1,
                "header {header} was not recognized"
            );
        }
    }

    #[test]
    fn a_repeated_header_starts_the_section_over() {
        let description = parse_description(
            "[dependencies]\n\
             gid:first:jar:1.0\n\
             [dependencies]\n\
             gid:second:jar:1.0\n",
        )
        .unwrap();
        assert_eq!(description.dependencies.len(), 1);
        assert_eq!(description.dependencies[0].artifact_id, "second");
    }

    #[test]
    fn relocation_is_a_single_coordinate() {
        let description = parse_description("[relocation]\nnew:aid:jar:2.0\n").unwrap();
        let relocation = description.relocation.as_ref().unwrap();
        assert_eq!(relocation.group_id, "new");
        assert_eq!(relocation.version, "2.0");
    }

    #[test]
    fn repositories_keep_the_colons_in_their_url() {
        let description =
            parse_description("[repositories]\nid:default:http://example.com:8080/repo\n").unwrap();
        let repository = &description.repositories[0];
        assert_eq!(repository.id, "id");
        assert_eq!(repository.type_, "default");
        assert_eq!(repository.url, "http://example.com:8080/repo");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let description = parse_description(
            "# leading comment\n\
             \n\
             [dependencies]  # trailing\n\
             gid:aid:jar:1.0   # another\n",
        )
        .unwrap();
        assert_eq!(description.dependencies.len(), 1);
    }

    #[test]
    fn content_before_a_header_is_an_error() {
        let error = parse_description("gid:aid:jar:1.0\n").unwrap_err();
        assert!(matches!(error, IniError::NoSection { .. }));
    }

    #[test]
    fn too_few_coordinate_fields_is_an_error() {
        let error = parse_description("[dependencies]\ngid:aid:1.0\n").unwrap_err();
        assert!(matches!(error, IniError::Coordinates { .. }));
    }

    #[test]
    fn an_unmodelled_scope_is_recorded_rather_than_rejected() {
        // Maven's scope is a free string; jv's is an enum. One corpus file uses
        // a synthetic marker scope, and losing it silently would be worse than
        // either failing or reporting it.
        let description =
            parse_description("[managed-dependencies]\ngid:aid:ext:ver:managedScope\n").unwrap();
        assert_eq!(description.unmodelled_scopes, vec!["managedScope"]);
        assert_eq!(description.managed_dependencies[0].scope, None);
    }

    #[test]
    fn an_unknown_section_is_ignored() {
        let description =
            parse_description("[something-else]\nwhatever\n[dependencies]\ngid:a:jar:1\n").unwrap();
        assert_eq!(description.dependencies.len(), 1);
    }

    #[test]
    fn descriptor_file_names_leave_coordinates_unescaped() {
        let reader = DescriptorReader::new("/corpus");
        let artifact = Artifact {
            group_id: "gid".to_owned(),
            artifact_id: "b-alt".to_owned(),
            version: "1.0".to_owned(),
            classifier: String::new(),
            extension: "jar".to_owned(),
        };
        assert_eq!(
            reader.path_of(&artifact),
            Path::new("/corpus/gid_b-alt_1.0.ini")
        );
    }
}
