//! The effective-POM pipeline.
//!
//! Implements Maven 3.9's ordering, which differs from Maven 4's in two ways that
//! change results: 3.9 injects each POM's own active profiles **before**
//! inheritance (Maven 4 does it after), and it activates a POM's profiles **once**
//! rather than twice. See `docs/spec/effective-pom.md` §1.5 and §10.
//!
//! ```text
//! phase 1
//!   activate settings profiles; fold their properties into the user properties
//!   walk the lineage, starting at the POM itself:
//!       resolve CI-friendly versions (${revision}, ${sha1}, ${changelist})
//!       collapse duplicate declarations
//!       activate this POM's profiles against its own properties
//!       inject those profiles into this POM
//!       (root only) inject the active settings profiles
//!       read the parent; stop at the super POM
//!   assemble inheritance from the super POM downwards
//!   interpolate once
//!   normalize URLs
//! phase 2
//!   translate relative build paths against the project directory
//!   inject pluginManagement
//!   inject the lifecycle bindings (opt-in)
//!   import BOMs
//!   inject dependencyManagement
//!   materialize the compile scope default
//! ```
//!
//! Lifecycle bindings come *after* pluginManagement injection, and that is not an
//! arrangement of convenience: the injection pass can only reach plugins already
//! in `<plugins>`, so putting it first is what leaves the merge in
//! [`crate::lifecycle`] responsible for letting a managed version beat the
//! version a lifecycle binds.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jv_model::{Dependency, Model, Profile, SettingsProfile, parse_pom};

use crate::activation::{ActivationContext, ProfileSource, select_active_profiles};
use crate::context::BuildContext;
use crate::interpolate::{interpolate_model, replace_ci_friendly_version};
use crate::lifecycle::inject_lifecycle_bindings;
use crate::management::{
    extract_bom_imports, inject_default_values, inject_dependency_management,
    inject_plugin_management, merge_duplicates, merge_imported_management,
};
use crate::merge::assemble_inheritance;
use crate::problem::{BuildError, Problem};
use crate::source::{ModelSource, SourcedModel};

/// The user property Maven seeds so `<activation><property><name>packaging</name>`
/// has something to match against.
const PACKAGING_PROPERTY: &str = "packaging";

/// Bounds the parent chain. Real chains are single digits deep; anything near
/// this is a cycle the identity checks somehow missed.
const MAX_LINEAGE: usize = 64;

/// Bounds nested BOM imports, for the same reason and with more urgency: the
/// import path recurses, so an unbounded chain is a stack overflow rather than a
/// slow build. Real nesting is two or three deep.
const MAX_IMPORT_DEPTH: usize = 64;

/// Maven 3.9's super POM, the implicit root of every parent chain.
///
/// Embedded verbatim from `maven-model-builder-3.9.9.jar`
/// (`org/apache/maven/model/pom-4.0.0.xml`, Apache-2.0). It is the authoritative
/// artifact rather than a transcription, and it matters: `central` is declared
/// *here*, not in Maven's code, so without it no POM has any repository at all.
/// Note that Maven 4's super POM dropped `central`, which is why the version
/// shipped in the reference clone cannot be used for a 3.9 target.
const SUPER_POM: &str = include_str!("../resources/super-pom-3.9.9.xml");

fn super_pom() -> &'static Model {
    static PARSED: OnceLock<Model> = OnceLock::new();
    PARSED.get_or_init(|| {
        parse_pom(SUPER_POM)
            .expect("the embedded super POM must parse")
            .model
    })
}

/// A fully built effective model.
#[derive(Clone, Debug)]
pub struct EffectiveModel {
    pub model: Model,
    /// Ids of every profile that ended up active, POM and settings alike.
    pub active_profiles: Vec<String>,
    /// The parent chain, nearest first, ending at the super POM.
    pub lineage: Vec<String>,
    /// Everything questionable found on the way. Never fatal on its own.
    pub problems: Vec<Problem>,
}

impl EffectiveModel {
    /// The problems worth showing a user by default.
    pub fn errors(&self) -> impl Iterator<Item = &Problem> {
        self.problems
            .iter()
            .filter(|problem| problem.severity == crate::problem::Severity::Error)
    }
}

/// Turns raw POMs into effective models.
pub struct ModelBuilder<'a> {
    source: &'a dyn ModelSource,
    context: BuildContext,
    external_profiles: Vec<Profile>,
    lifecycle_bindings: bool,
}

impl std::fmt::Debug for ModelBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelBuilder")
            .field("context", &self.context)
            .field("external_profiles", &self.external_profiles.len())
            .field("lifecycle_bindings", &self.lifecycle_bindings)
            .finish_non_exhaustive()
    }
}

impl<'a> ModelBuilder<'a> {
    pub fn new(source: &'a dyn ModelSource, context: BuildContext) -> Self {
        Self {
            source,
            context,
            external_profiles: Vec::new(),
            lifecycle_bindings: false,
        }
    }

    /// Adds the profiles declared in `settings.xml`.
    ///
    /// These are narrower than POM profiles — properties and repositories only —
    /// and their `activeByDefault` is never suppressed by POM profile activation.
    pub fn with_settings_profiles(mut self, profiles: &[SettingsProfile]) -> Self {
        self.external_profiles = profiles.iter().map(settings_profile_to_profile).collect();
        self
    }

    /// Adds the plugins the packaging's lifecycle binds, the way Maven does for a
    /// request with `processPlugins` set.
    ///
    /// Off by default, matching `DefaultModelBuildingRequest`: resolving
    /// dependencies never reads `<build><plugins>`, so `jv tree` and `jv resolve`
    /// would be paying for a plugin list nothing looks at. `jv sync` needs it,
    /// because the plugins are artifacts a later `mvn -o` will demand from the
    /// local repository.
    pub fn with_lifecycle_bindings(mut self, enabled: bool) -> Self {
        self.lifecycle_bindings = enabled;
        self
    }

    /// Builds the effective model for an already-read POM.
    pub fn build(&self, root: SourcedModel) -> Result<EffectiveModel, BuildError> {
        self.build_with_import_chain(root, &mut Vec::new(), self.lifecycle_bindings)
    }

    /// The pipeline, threading the BOM-import chain so that a BOM's own imports
    /// cannot loop back through an outer level.
    fn build_with_import_chain(
        &self,
        root: SourcedModel,
        import_ids: &mut Vec<String>,
        lifecycle_bindings: bool,
    ) -> Result<EffectiveModel, BuildError> {
        let mut problems = Vec::new();
        let mut active_profiles = Vec::new();

        // Settings profiles are activated first, and their properties join the
        // user properties so that everything downstream — including POM profile
        // activation — sees them.
        let mut context = self.context.clone();
        let external_active = {
            let properties = root.model.properties.clone();
            let activation = ActivationContext {
                context: &self.context,
                basedir: root.basedir.as_deref(),
                model_properties: &properties,
            };
            select_active_profiles(
                &self.external_profiles,
                ProfileSource::Settings,
                &activation,
                &mut problems,
                "settings.xml",
            )
        };
        for index in &external_active {
            let profile = &self.external_profiles[*index];
            active_profiles.push(profile.id_or_default().to_owned());
            for (key, value) in &profile.properties {
                context.user_properties.insert(key.clone(), value.clone());
            }
        }

        // `<property><name>packaging</name>` is activated against a property
        // Maven seeds rather than one anybody declares:
        // `getProfileActivationContext` does
        // `userProperties.computeIfAbsent("packaging", …)`. Without it that
        // activator never fires at all.
        //
        // It is the *root* POM's packaging, for the whole lineage — so a
        // parent's profile testing `packaging=jar` activates while building a
        // jar child of a pom parent. `computeIfAbsent` also means an explicit
        // `-Dpackaging` wins, which is why this does not overwrite.
        context
            .user_properties
            .entry(PACKAGING_PROPERTY.to_owned())
            .or_insert_with(|| root.model.packaging_or_default().to_owned());

        // Phase 1: walk the lineage, transforming each model in place.
        let basedir = root.basedir.clone();
        let mut lineage: Vec<SourcedModel> = Vec::new();
        let mut lineage_names: Vec<String> = Vec::new();
        let mut visited: Vec<String> = Vec::new();
        let mut current = Some(root);
        let mut used_super_pom = false;

        while let Some(mut model) = current.take() {
            if lineage.len() >= MAX_LINEAGE {
                return Err(BuildError::ParentCycle {
                    coordinates: lineage_names.first().cloned().unwrap_or_default(),
                    repeated: model.coordinates(),
                });
            }

            let identity = model.coordinates();
            // The super POM has no coordinates, so it is tracked separately.
            if !used_super_pom {
                if visited.contains(&identity) {
                    return Err(BuildError::ParentCycle {
                        coordinates: lineage_names.first().cloned().unwrap_or(identity.clone()),
                        repeated: identity,
                    });
                }
                visited.push(identity.clone());
            }

            replace_ci_friendly_version(&mut model.model, &context);
            merge_duplicates(&mut model.model);

            // Profiles are activated against *this* model's properties, and
            // injected into it before it is inherited from.
            let own_properties = model.model.properties.clone();
            let activation = ActivationContext {
                context: &context,
                // The directory of the POM being *built*, for every model in the
                // lineage — not each model's own. Maven sets `projectDirectory`
                // once from the request's POM file, so a parent's
                // `<file><exists>marker.txt</exists></file>` looks for
                // `marker.txt` beside the child, and `${basedir}` in an
                // ancestor's activation means the child's directory too.
                // Verified: a marker file next to the parent does *not* activate
                // that profile when Maven builds the child.
                basedir: basedir.as_deref(),
                model_properties: &own_properties,
            };
            let active = select_active_profiles(
                &model.model.profiles,
                ProfileSource::Pom,
                &activation,
                &mut problems,
                &model.source,
            );
            let chosen: Vec<Profile> = active
                .iter()
                .map(|index| model.model.profiles[*index].clone())
                .collect();
            for profile in &chosen {
                active_profiles.push(profile.id_or_default().to_owned());
                crate::merge::inject_profile(&mut model.model, profile);
            }

            // The settings profiles apply to the POM being built, not to its
            // ancestors.
            if lineage.is_empty() {
                for index in &external_active {
                    crate::merge::inject_profile(&mut model.model, &self.external_profiles[*index]);
                }
            }

            lineage_names.push(model.source.clone());
            let parent_request = model.model.parent.clone();
            let model_basedir = model.basedir.clone();
            lineage.push(model);

            if used_super_pom {
                break;
            }
            current = match parent_request {
                Some(parent) => {
                    Some(self.read_parent(&parent, model_basedir.as_deref(), &mut problems)?)
                }
                None => {
                    used_super_pom = true;
                    Some(SourcedModel::new(super_pom().clone(), "super POM"))
                }
            };
        }

        // Assemble inheritance from the top of the chain downwards.
        let mut assembled = lineage
            .last()
            .expect("the lineage always holds at least the root")
            .model
            .clone();
        for ancestor_index in (0..lineage.len() - 1).rev() {
            let child = &lineage[ancestor_index];
            let mut merged = child.model.clone();
            let directory_name = child
                .basedir
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str());
            assemble_inheritance(&mut merged, &assembled, directory_name);
            assembled = merged;
        }

        let source_name = lineage_names
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());

        // A single interpolation pass, after inheritance.
        interpolate_model(
            &mut assembled,
            basedir.as_deref(),
            &context,
            &source_name,
            &mut problems,
        );
        normalize_urls(&mut assembled);

        // Phase 2.
        translate_paths(&mut assembled, basedir.as_deref());
        inject_plugin_management(&mut assembled);
        if lifecycle_bindings {
            inject_lifecycle_bindings(&mut assembled, &source_name, &mut problems);
        }
        self.import_boms(&mut assembled, &source_name, import_ids, &mut problems);
        inject_dependency_management(&mut assembled);
        inject_default_values(&mut assembled);

        if assembled.artifact_id.is_none() {
            return Err(BuildError::NoArtifactId {
                coordinates: source_name,
            });
        }

        // Last, on the effective model, where Maven validates: `${...}` has to
        // be resolved and management applied before a coordinate can be judged.
        problems.extend(crate::validate::validate(&mut assembled, &source_name));

        Ok(EffectiveModel {
            model: assembled,
            active_profiles,
            lineage: lineage_names,
            problems,
        })
    }

    /// Reads a POM's parent, trying `<relativePath>` before the repository.
    fn read_parent(
        &self,
        parent: &jv_model::Parent,
        basedir: Option<&Path>,
        problems: &mut Vec<Problem>,
    ) -> Result<SourcedModel, BuildError> {
        let group_id = parent.group_id.as_deref().unwrap_or_default();
        let artifact_id = parent.artifact_id.as_deref().unwrap_or_default();
        let version = parent.version.as_deref().unwrap_or_default();
        let coordinates = format!("{group_id}:{artifact_id}:{version}");

        // A POM next door is authoritative, but only when it really is this
        // parent: Maven falls back to the repository when the coordinates
        // disagree, which is what lets a module sit beside an unrelated POM.
        if let (Some(basedir), Some(relative)) = (basedir, parent.effective_relative_path()) {
            // `<relativePath>` is nearly always `../pom.xml`, so the joined path
            // has to be normalized before it can be looked up or reported.
            let candidate = normalize_lexically(&basedir.join(relative));
            match self.source.get_at_path(&candidate) {
                Ok(Some(candidate)) => {
                    if parent_matches(&candidate.model, group_id, artifact_id, version) {
                        return Ok(candidate);
                    }
                    problems.push(Problem::warning(
                        &candidate.source,
                        format!(
                            "the POM at <relativePath> {relative} is not {coordinates}; \
                             looking in the repositories instead"
                        ),
                    ));
                }
                Ok(None) => {}
                Err(reason) => problems.push(Problem::warning(
                    relative,
                    format!("cannot read the parent at <relativePath>: {reason}"),
                )),
            }
        }

        self.source
            .get(group_id, artifact_id, version)
            .map_err(|reason| BuildError::MissingModel {
                coordinates,
                reason,
            })
    }

    /// Resolves `<scope>import</scope>` entries, recursively.
    ///
    /// Each BOM is built as a full effective model — its own parent chain,
    /// interpolation and imports — and only its `<dependencyManagement>` is kept.
    /// Cycles are detected by coordinates with no depth limit, matching upstream.
    fn import_boms(
        &self,
        model: &mut Model,
        source_name: &str,
        import_ids: &mut Vec<String>,
        problems: &mut Vec<Problem>,
    ) {
        let imports = extract_bom_imports(model, source_name, problems);
        if imports.is_empty() {
            return;
        }

        // Bounded like the parent chain is. A BOM that imports a BOM that
        // imports a BOM recurses through `build_bom`, and a repository serving a
        // long enough chain overflowed the stack — which with `panic = "abort"`
        // is a process abort an embedder cannot catch, not a build failure.
        // Maven throws a `StackOverflowError` and reports it; jv reports it
        // before the stack runs out.
        if import_ids.len() >= MAX_IMPORT_DEPTH {
            problems.push(Problem::error(
                source_name,
                format!(
                    "dependencies of type=pom and scope=import are nested more than \
                     {MAX_IMPORT_DEPTH} deep; the rest were not read"
                ),
            ));
            return;
        }

        let own = format!(
            "{}:{}:{}",
            model.declared_or_parent_group_id().unwrap_or_default(),
            model.artifact_id.as_deref().unwrap_or_default(),
            model.declared_or_parent_version().unwrap_or_default()
        );
        import_ids.push(own);

        let mut imported: Vec<Vec<Dependency>> = Vec::new();
        for entry in &imports {
            let coordinates = format!(
                "{}:{}:{}",
                entry.group_id,
                entry.artifact_id,
                entry.version.as_deref().unwrap_or_default()
            );
            if import_ids.contains(&coordinates) {
                problems.push(Problem::error(
                    source_name,
                    format!(
                        "the dependencies of type=pom and scope=import form a cycle: \
                         {coordinates}"
                    ),
                ));
                continue;
            }
            match self.build_bom(entry, import_ids, problems) {
                Some(management) => imported.push(management),
                None => problems.push(Problem::error(
                    source_name,
                    format!("non-resolvable import POM {coordinates}"),
                )),
            }
        }

        import_ids.pop();
        merge_imported_management(model, &imported);
    }

    /// Builds one BOM and returns its managed dependencies.
    fn build_bom(
        &self,
        entry: &Dependency,
        import_ids: &mut Vec<String>,
        problems: &mut Vec<Problem>,
    ) -> Option<Vec<Dependency>> {
        let version = entry.version.as_deref()?;
        let sourced = match self
            .source
            .get(&entry.group_id, &entry.artifact_id, version)
        {
            Ok(sourced) => sourced,
            Err(reason) => {
                problems.push(Problem::error(
                    format!("{}:{}:{version}", entry.group_id, entry.artifact_id),
                    reason,
                ));
                return None;
            }
        };

        // A BOM is built like any other POM, sharing the outer cycle-detection
        // chain so that a loop spanning two levels is still caught. Its lifecycle
        // bindings are never injected: only its `<dependencyManagement>` is read,
        // and upstream builds an imported POM with plugin processing off too.
        let mut built = match self.build_with_import_chain(sourced, import_ids, false) {
            Ok(built) => built,
            Err(error) => {
                problems.push(Problem::error(
                    format!("{}:{}:{version}", entry.group_id, entry.artifact_id),
                    error.to_string(),
                ));
                return None;
            }
        };
        problems.append(&mut built.problems);
        Some(built.model.dependency_management)
    }
}

/// Resolves `.` and `..` without touching the filesystem.
///
/// `Path::join` leaves `..` in place, so a `<relativePath>` of `../pom.xml`
/// would otherwise be looked up as `<basedir>/../pom.xml` — a different string
/// from the file's real path, which breaks both lookups and error messages.
/// Normalizing lexically rather than canonicalizing keeps this working for paths
/// that do not exist, which matters for tests and for clear diagnostics.
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only a real directory name can be cancelled; a leading `..`
                // has to survive.
                let can_pop = normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));
                if can_pop {
                    normalized.pop();
                } else {
                    normalized.push("..");
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Whether a POM found on disk really is the declared parent.
fn parent_matches(model: &Model, group_id: &str, artifact_id: &str, version: &str) -> bool {
    model.artifact_id.as_deref() == Some(artifact_id)
        && model
            .declared_or_parent_group_id()
            .is_none_or(|found| found == group_id)
        && model
            .declared_or_parent_version()
            .is_none_or(|found| found == version)
}

/// Collapses `/../` in URL fields, the way `DefaultUrlNormalizer` does.
fn normalize_urls(model: &mut Model) {
    if let Some(url) = &model.url {
        model.url = Some(normalize_url(url));
    }
}

fn normalize_url(url: &str) -> String {
    let mut result = url.to_owned();
    while let Some(index) = result.find("/../") {
        if index == 0 {
            // A leading `/..` has nothing to cancel; drop it.
            result.replace_range(0..3, "");
            continue;
        }
        // Remove the segment before it, along with the `/../`.
        let preceding = result[..index].rfind('/').map_or(0, |slash| slash);
        result.replace_range(preceding..index + 3, "");
    }
    result
}

/// Makes relative build directories absolute against the project directory.
///
/// Interpolation already resolves the common `${project.basedir}/target` spelling;
/// this covers a bare `<directory>target</directory>`.
fn translate_paths(model: &mut Model, basedir: Option<&Path>) {
    let Some(basedir) = basedir else {
        return;
    };
    let Some(build) = &mut model.build else {
        return;
    };
    for value in [
        &mut build.source_directory,
        &mut build.script_source_directory,
        &mut build.test_source_directory,
        &mut build.output_directory,
        &mut build.test_output_directory,
        &mut build.directory,
    ]
    .into_iter()
    .flatten()
    {
        let path = Path::new(value.as_str());
        if path.is_relative() {
            *value = basedir.join(path).to_string_lossy().into_owned();
        }
    }
}

/// Reshapes a `settings.xml` profile as a POM profile so one activation and
/// injection implementation serves both.
fn settings_profile_to_profile(profile: &SettingsProfile) -> Profile {
    Profile {
        id: profile.id.clone(),
        activation: profile.activation.clone(),
        properties: profile.properties.clone(),
        repositories: profile.repositories.clone(),
        plugin_repositories: profile.plugin_repositories.clone(),
        // A settings profile cannot contribute these.
        dependencies: Vec::new(),
        dependency_management: Vec::new(),
        modules: Vec::new(),
        build: None,
        reporting_plugins: Vec::new(),
        distribution_management: None,
    }
}

/// Convenience for callers that just want an effective model from one POM.
pub fn build_effective_model(
    source: &dyn ModelSource,
    root: SourcedModel,
    context: BuildContext,
) -> Result<EffectiveModel, BuildError> {
    ModelBuilder::new(source, context).build(root)
}
