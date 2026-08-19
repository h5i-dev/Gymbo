//! Parent inheritance and profile injection.
//!
//! Both are the same merge with the dominance flipped, which is exactly how
//! upstream models it: `MavenModelMerger` is constructed once per direction, with
//! `sourceDominant = false` for parent → child and `true` for profile → model.
//! The generic rules, by field kind:
//!
//! | Kind | Rule |
//! |---|---|
//! | `String` | assign when the source has a value and either the source dominates or the target has none |
//! | `boolean` / `int` | assign only when the source dominates, so these are **never inherited** |
//! | list of strings | union, target order first |
//! | properties | the dominant side overlays the recessive |
//! | single association | recursive, per leaf field |
//! | list association | merge by key; on collision the dominant element **replaces the other wholesale** |
//!
//! The last rule is the one that surprises people: because upstream sets
//! `deepMerge = false`, a child dependency that collides with a parent's managed
//! entry does not inherit the parent's `version` or `exclusions` — the child's
//! element wins entirely.

use std::collections::BTreeSet;

use std::collections::HashMap;

use jv_model::{
    Build, Dependency, DistributionManagement, ManagementKey, Model, Plugin, PluginExecution,
    Profile, Properties, Repository,
};

/// The property a child uses to override the directory name appended to an
/// inherited URL. Deliberately never inherited itself.
const PROJECT_DIRECTORY_PROPERTY: &str = "project.directory";

/// Merges a parent model into a child, in place.
///
/// `child_directory_name` is the child's directory name on disk when known. It
/// only affects URL path appending, and upstream's MNG-5000 rule means a model
/// loaded from a repository and the same model loaded from a file can produce
/// different URLs — so the caller passes what it knows rather than this function
/// guessing.
pub fn assemble_inheritance(child: &mut Model, parent: &Model, child_directory_name: Option<&str>) {
    // `project.url` is inherited with the child's path appended. jv models
    // neither `<scm>` nor `<distributionManagement><site>`, so the other four
    // fields upstream adjusts have nothing to adjust here, and the
    // `child.*.inherit.append.path` attributes are not parsed — all of which
    // affect `${project.url}` only, never which artifacts get resolved.
    if child.url.is_none() {
        if let Some(parent_url) = parent.url.as_deref().filter(|url| !url.trim().is_empty()) {
            let child_path = child
                .properties
                .get(PROJECT_DIRECTORY_PROPERTY)
                .cloned()
                .or_else(|| child.artifact_id.clone())
                .unwrap_or_default();
            let adjustment =
                child_path_adjustment(parent, child_directory_name, &child_path, child);
            child.url = Some(append_path(parent_url, &adjustment, &child_path));
        }
    }

    // Never inherited: modelVersion, artifactId, name, packaging (the child
    // always has the schema default), profiles, modules.
    inherit_string(&mut child.group_id, &parent.group_id);
    inherit_string(&mut child.version, &parent.version);
    inherit_string(&mut child.description, &parent.description);
    inherit_string(&mut child.inception_year, &parent.inception_year);

    inherit_properties(&mut child.properties, &parent.properties);

    merge_dependencies(&mut child.dependencies, &parent.dependencies, false);
    merge_dependencies(
        &mut child.dependency_management,
        &parent.dependency_management,
        false,
    );

    merge_repositories(&mut child.repositories, &parent.repositories, false);
    merge_repositories(
        &mut child.plugin_repositories,
        &parent.plugin_repositories,
        false,
    );

    if let Some(parent_build) = &parent.build {
        let child_build = child.build.get_or_insert_with(Build::default);
        merge_build(child_build, parent_build, false);
    }

    // Report plugins inherit on the same terms as build plugins, which is why
    // they go through the same merge rather than a rule of their own.
    merge_plugins(
        &mut child.reporting_plugins,
        &parent.reporting_plugins,
        false,
    );

    if let Some(parent_management) = &parent.distribution_management {
        let child_management = child
            .distribution_management
            .get_or_insert_with(DistributionManagement::default);
        // `relocation` is explicitly not inherited; `status` follows the plain
        // string rule.
        inherit_string(&mut child_management.status, &parent_management.status);
    }
}

/// Merges an active profile into a model, in place, with the profile dominant.
pub fn inject_profile(model: &mut Model, profile: &Profile) {
    dominate_properties(&mut model.properties, &profile.properties);
    union_strings(&mut model.modules, &profile.modules);

    merge_dependencies(&mut model.dependencies, &profile.dependencies, true);
    merge_dependencies(
        &mut model.dependency_management,
        &profile.dependency_management,
        true,
    );

    merge_repositories(&mut model.repositories, &profile.repositories, true);
    merge_repositories(
        &mut model.plugin_repositories,
        &profile.plugin_repositories,
        true,
    );

    if let Some(profile_build) = &profile.build {
        let build = model.build.get_or_insert_with(Build::default);
        merge_build(build, profile_build, true);
    }

    merge_plugins(
        &mut model.reporting_plugins,
        &profile.reporting_plugins,
        true,
    );

    if let Some(profile_management) = &profile.distribution_management {
        let management = model
            .distribution_management
            .get_or_insert_with(DistributionManagement::default);
        dominate_string(&mut management.status, &profile_management.status);
        if profile_management.relocation.is_some() {
            management.relocation = profile_management.relocation.clone();
        }
    }
}

// ------------------------------------------------------------- generic rules

/// The string rule with the target dominant: assign only into a gap.
fn inherit_string(target: &mut Option<String>, source: &Option<String>) {
    if target.is_none() {
        if let Some(value) = source {
            *target = Some(value.clone());
        }
    }
}

/// The string rule with the source dominant.
fn dominate_string(target: &mut Option<String>, source: &Option<String>) {
    if let Some(value) = source {
        *target = Some(value.clone());
    }
}

/// Applies the string rule in whichever direction is asked for.
fn merge_string(target: &mut Option<String>, source: &Option<String>, source_dominant: bool) {
    if source_dominant {
        dominate_string(target, source);
    } else {
        inherit_string(target, source);
    }
}

/// Boolean and integer fields are assigned only by a dominant source, which is
/// why `<inherited>`, `<extensions>` and friends are never inherited.
fn merge_flag(target: &mut Option<bool>, source: &Option<bool>, source_dominant: bool) {
    if source_dominant {
        if let Some(value) = source {
            *target = Some(*value);
        }
    }
}

/// Union of two string lists, target order first, duplicates dropped.
fn union_strings(target: &mut Vec<String>, source: &[String]) {
    let existing: BTreeSet<&String> = target.iter().collect();
    let additions: Vec<String> = source
        .iter()
        .filter(|value| !existing.contains(value))
        .cloned()
        .collect();
    target.extend(additions);
}

/// Properties inheritance: the parent's entries come first, minus the child
/// directory property, and the child's overlay them.
fn inherit_properties(child: &mut Properties, parent: &Properties) {
    let mut merged = Properties::new();
    for (key, value) in parent {
        if key != PROJECT_DIRECTORY_PROPERTY {
            merged.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in child.iter() {
        merged.insert(key.clone(), value.clone());
    }
    *child = merged;
}

fn dominate_properties(target: &mut Properties, source: &Properties) {
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}

// ------------------------------------------------------------- dependencies

/// Merges dependency lists by management key.
///
/// Target order is preserved and source-only entries are appended. On a
/// collision the dominant element replaces the other outright — no per-field
/// merge, so a child declaration does not pick up the parent's `version`.
///
/// Indexed rather than scanned. This is the hottest loop in the whole pipeline:
/// it runs once per POM in every artifact's lineage, and a Spring Boot project
/// inherits `spring-boot-dependencies`' five hundred managed entries at every
/// one of them. The scan was O(n·m) with five `String` allocations per probe —
/// 26 ms of the 40 ms it took to build one effective model, against 0.2 ms for
/// the interpolation pass everyone assumes is the expensive part.
fn merge_dependencies(target: &mut Vec<Dependency>, source: &[Dependency], source_dominant: bool) {
    if source.is_empty() {
        return;
    }

    // The target's own duplicates collapse first — last value, first position —
    // because Maven's generated merger seeds its list with
    // `mergeAll(target, sourceDominant=true)` before merging the source in.
    //
    // Only when there *is* a source, which is not an optimization but the
    // observed rule: with an inherited `<dependencyManagement>` a POM declaring
    // `d:d` twice resolves to the second, and with nothing inherited it resolves
    // to the first, because then this merge never runs. Both checked against
    // 3.9.9. `<dependencies>` never reaches here with duplicates — phase 1's
    // `merge_duplicates` has already collapsed those — so this is management's
    // rule in practice.
    collapse_duplicates(target);

    // First occurrence wins, which is what `Iterator::find` did.
    let mut at: HashMap<ManagementKey, usize> = HashMap::with_capacity(target.len());
    for (index, existing) in target.iter().enumerate() {
        at.entry(existing.management_key()).or_insert(index);
    }

    for candidate in source {
        let key = candidate.management_key();
        match at.get(&key) {
            Some(index) => {
                if source_dominant {
                    target[*index] = candidate.clone();
                }
            }
            None => {
                // Indexed as it is appended, so a source list carrying the same
                // key twice collides with the first copy rather than appending
                // it again.
                at.insert(key, target.len());
                target.push(candidate.clone());
            }
        }
    }
}

/// Keeps one entry per management key: the last value, at the first position.
fn collapse_duplicates(entries: &mut Vec<Dependency>) {
    let mut at: HashMap<ManagementKey, usize> = HashMap::with_capacity(entries.len());
    let mut collapsed: Vec<Dependency> = Vec::with_capacity(entries.len());
    for entry in std::mem::take(entries) {
        match at.get(&entry.management_key()) {
            Some(index) => collapsed[*index] = entry,
            None => {
                at.insert(entry.management_key(), collapsed.len());
                collapsed.push(entry);
            }
        }
    }
    *entries = collapsed;
}

// ------------------------------------------------------------- repositories

fn repository_id(repository: &Repository) -> &str {
    repository.id.as_deref().unwrap_or("")
}

/// Merges repository lists by id, **dominant list first**.
///
/// The ordering is the subtle part: under inheritance the child dominates and so
/// keeps its position, but a profile that contributes repositories puts them
/// *ahead* of the model's own. Repository order decides which one is consulted
/// first, so this is not cosmetic.
fn merge_repositories(target: &mut Vec<Repository>, source: &[Repository], source_dominant: bool) {
    if source_dominant {
        let mut merged: Vec<Repository> = source.to_vec();
        for existing in target.iter() {
            if !merged
                .iter()
                .any(|candidate| repository_id(candidate) == repository_id(existing))
            {
                merged.push(existing.clone());
            }
        }
        *target = merged;
    } else {
        for candidate in source {
            if !target
                .iter()
                .any(|existing| repository_id(existing) == repository_id(candidate))
            {
                target.push(candidate.clone());
            }
        }
    }
}

// ------------------------------------------------------------- build

fn merge_build(target: &mut Build, source: &Build, source_dominant: bool) {
    for (target_slot, source_slot) in [
        (&mut target.source_directory, &source.source_directory),
        (
            &mut target.script_source_directory,
            &source.script_source_directory,
        ),
        (
            &mut target.test_source_directory,
            &source.test_source_directory,
        ),
        (&mut target.output_directory, &source.output_directory),
        (
            &mut target.test_output_directory,
            &source.test_output_directory,
        ),
        (&mut target.directory, &source.directory),
        (&mut target.final_name, &source.final_name),
        (&mut target.default_goal, &source.default_goal),
    ] {
        merge_string(target_slot, source_slot, source_dominant);
    }

    // Extensions are keyed by groupId:artifactId, target first.
    for candidate in &source.extensions {
        let key = (
            candidate.group_id.clone().unwrap_or_default(),
            candidate.artifact_id.clone().unwrap_or_default(),
        );
        let existing = target.extensions.iter_mut().find(|extension| {
            (
                extension.group_id.clone().unwrap_or_default(),
                extension.artifact_id.clone().unwrap_or_default(),
            ) == key
        });
        match existing {
            Some(slot) => {
                if source_dominant {
                    *slot = candidate.clone();
                }
            }
            None => target.extensions.push(candidate.clone()),
        }
    }

    merge_plugins(&mut target.plugins, &source.plugins, source_dominant);
    merge_plugins(
        &mut target.plugin_management,
        &source.plugin_management,
        source_dominant,
    );
}

// ------------------------------------------------------------- plugins

/// One output slot of the plugin merge.
struct PluginSlot {
    key: String,
    plugin: Plugin,
    /// Target-only plugins that appeared before this key and keep their relative
    /// position ahead of it.
    predecessors: Vec<Plugin>,
}

/// Merges plugin lists.
///
/// Unlike every other list, plugin order is decided by the *source* under
/// inheritance: the parent's declaration order dominates, and child-only plugins
/// keep their position relative to the parent-derived ones. Profile injection is
/// the mirror image, with the model's order dominating.
fn merge_plugins(target: &mut Vec<Plugin>, source: &[Plugin], source_dominant: bool) {
    if source.is_empty() {
        return;
    }

    // Seed the ordered slots from whichever side dominates the ordering. Under
    // inheritance that is the parent (the source); under profile injection it is
    // the model (the target).
    let (ordering_side, merging_side): (Vec<Plugin>, Vec<Plugin>) = if source_dominant {
        (target.clone(), source.to_vec())
    } else {
        (source.to_vec(), target.clone())
    };

    let mut slots: Vec<PluginSlot> = Vec::new();
    for plugin in &ordering_side {
        if !source_dominant {
            // Inheritance admits a parent plugin only when it is inherited, or
            // when it carries executions that may individually opt back in.
            if !plugin.is_inherited() && plugin.executions.is_empty() {
                continue;
            }
            // Merging into a fresh instance is what lets execution-level
            // inheritance run rather than copying the parent wholesale.
            let mut fresh = Plugin::default();
            merge_plugin(&mut fresh, plugin, false);
            slots.push(PluginSlot {
                key: plugin.key(),
                plugin: fresh,
                predecessors: Vec::new(),
            });
        } else {
            slots.push(PluginSlot {
                key: plugin.key(),
                plugin: plugin.clone(),
                predecessors: Vec::new(),
            });
        }
    }

    let mut buffered: Vec<Plugin> = Vec::new();
    for plugin in &merging_side {
        let key = plugin.key();
        match slots.iter_mut().find(|slot| slot.key == key) {
            Some(slot) => {
                if source_dominant {
                    // The profile's plugin wins over the model's.
                    let mut merged = slot.plugin.clone();
                    merge_plugin(&mut merged, plugin, true);
                    slot.plugin = merged;
                } else {
                    // The child's plugin is the target; the parent fills gaps.
                    let mut merged = plugin.clone();
                    merge_plugin(&mut merged, &slot.plugin, false);
                    slot.plugin = merged;
                }
                slot.predecessors = std::mem::take(&mut buffered);
            }
            None => buffered.push(plugin.clone()),
        }
    }

    let mut merged: Vec<Plugin> = Vec::new();
    for slot in slots {
        merged.extend(slot.predecessors);
        merged.push(slot.plugin);
    }
    merged.extend(buffered);
    *target = merged;
}

/// Merges one plugin into another.
pub(crate) fn merge_plugin(target: &mut Plugin, source: &Plugin, source_dominant: bool) {
    // Under inheritance a parent plugin marked not-inherited contributes none of
    // its own container fields, only its opted-in executions.
    if source_dominant || source.is_inherited() {
        merge_flag(&mut target.inherited, &source.inherited, source_dominant);
    }
    merge_string(&mut target.group_id, &source.group_id, source_dominant);
    merge_string(
        &mut target.artifact_id,
        &source.artifact_id,
        source_dominant,
    );
    merge_string(&mut target.version, &source.version, source_dominant);
    // `extensions` is String-typed in the model, so it follows the string rule
    // rather than the boolean one.
    if source_dominant || target.extensions.is_none() {
        if let Some(value) = source.extensions {
            target.extensions = Some(value);
        }
    }
    merge_dependencies(
        &mut target.dependencies,
        &source.dependencies,
        source_dominant,
    );
    // A union, not a merge, and deliberately not `merge_dependencies`.
    //
    // Maven merges `<configuration>` element by element, so a child that
    // redeclares `<annotationProcessorPaths>` replaces the parent's rather than
    // adding to it. This list cannot reproduce that: it is flattened, so the
    // structure that would say which element replaced which is gone. Keeping
    // both sides means jv may download a processor the effective configuration
    // turns out not to use, which costs a download; dropping the parent's would
    // mean `mvn -o` cannot find one it does use, which costs the build.
    for dependency in &source.configuration_artifacts {
        if !target.configuration_artifacts.contains(dependency) {
            target.configuration_artifacts.push(dependency.clone());
        }
    }
    merge_executions(target, source, source_dominant);
}

fn merge_executions(target: &mut Plugin, source: &Plugin, source_dominant: bool) {
    // Source executions come first when the source dominates ordering, matching
    // the plugin-level rule.
    let admitted: Vec<PluginExecution> = source
        .executions
        .iter()
        .filter(|execution| source_dominant || execution.is_inherited(source))
        .cloned()
        .collect();

    if source_dominant {
        for candidate in &admitted {
            let id = candidate.id_or_default().to_owned();
            match target
                .executions
                .iter_mut()
                .find(|existing| existing.id_or_default() == id)
            {
                Some(slot) => merge_execution(slot, candidate, true),
                None => target.executions.push(candidate.clone()),
            }
        }
    } else {
        // Inheritance inserts admitted parent executions first, then folds the
        // child's over them.
        let mut merged: Vec<PluginExecution> = admitted;
        for own in &target.executions {
            let id = own.id_or_default().to_owned();
            match merged
                .iter_mut()
                .find(|existing| existing.id_or_default() == id)
            {
                Some(slot) => {
                    let mut child = own.clone();
                    merge_execution(&mut child, slot, false);
                    *slot = child;
                }
                None => merged.push(own.clone()),
            }
        }
        target.executions = merged;
    }
}

fn merge_execution(target: &mut PluginExecution, source: &PluginExecution, source_dominant: bool) {
    merge_string(&mut target.id, &source.id, source_dominant);
    merge_string(&mut target.phase, &source.phase, source_dominant);
    merge_flag(&mut target.inherited, &source.inherited, source_dominant);
    union_strings(&mut target.goals, &source.goals);
}

// ------------------------------------------------------------- url appending

/// Derives the path segment inserted between the parent URL and the child's own
/// directory, from the parent's `<modules>` entries.
///
/// The filesystem is never consulted: the answer comes from matching a module
/// entry's last segment against the child's name.
fn child_path_adjustment(
    parent: &Model,
    child_directory_name: Option<&str>,
    child_path: &str,
    child: &Model,
) -> String {
    // MNG-5000: the directory name on disk wins over the artifactId when known,
    // which is why a repository-loaded parent can produce a different URL.
    let child_name = child_directory_name
        .map(str::to_owned)
        .or_else(|| child.artifact_id.clone())
        .unwrap_or_default();

    for module in &parent.modules {
        let mut entry = module.replace('\\', "/");
        // An entry may point straight at a POM file.
        if entry.to_ascii_lowercase().ends_with(".xml") {
            match entry.rfind('/') {
                Some(index) => entry.truncate(index + 1),
                None => entry.clear(),
            }
        }
        let entry = entry.strip_suffix('/').unwrap_or(&entry);
        let Some(last_separator) = entry.rfind('/') else {
            // No separator means no nesting, so no adjustment to make.
            continue;
        };
        let module_name = &entry[last_separator + 1..];
        if module_name == child_name || module_name == child_path {
            return entry[..last_separator].to_owned();
        }
    }
    String::new()
}

/// Appends path components to a URL, preserving a trailing slash if it had one.
fn append_path(base: &str, adjustment: &str, child_path: &str) -> String {
    let mut url = base.to_owned();
    for component in [adjustment, child_path] {
        if component.is_empty() {
            continue;
        }
        concat_path(&mut url, component);
    }
    url
}

fn concat_path(url: &mut String, path: &str) {
    let had_trailing_slash = url.ends_with('/');
    if path.starts_with('/') && had_trailing_slash {
        url.pop();
    } else if !path.starts_with('/') && !had_trailing_slash {
        url.push('/');
    }
    url.push_str(path);
    if had_trailing_slash && !path.ends_with('/') {
        url.push('/');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Scope, parse_pom};

    fn model_of(xml: &str) -> Model {
        parse_pom(xml).expect("parse").model
    }

    fn inherit(child_xml: &str, parent_xml: &str) -> Model {
        let mut child = model_of(child_xml);
        let parent = model_of(parent_xml);
        assemble_inheritance(&mut child, &parent, None);
        child
    }

    #[test]
    fn coordinates_fill_gaps_only() {
        let child = inherit(
            r#"<project><artifactId>child</artifactId></project>"#,
            r#"<project><groupId>g</groupId><artifactId>parent</artifactId><version>1.0</version></project>"#,
        );
        assert_eq!(child.group_id.as_deref(), Some("g"));
        assert_eq!(child.version.as_deref(), Some("1.0"));
        // artifactId and name are never inherited.
        assert_eq!(child.artifact_id.as_deref(), Some("child"));
    }

    #[test]
    fn name_is_never_inherited() {
        let child = inherit(
            r#"<project><artifactId>child</artifactId></project>"#,
            r#"<project><name>Parent Project</name></project>"#,
        );
        assert_eq!(child.name, None);
    }

    #[test]
    fn modules_are_never_inherited() {
        let child = inherit(
            r#"<project><artifactId>child</artifactId></project>"#,
            r#"<project><modules><module>a</module></modules></project>"#,
        );
        assert!(child.modules.is_empty());
    }

    #[test]
    fn properties_merge_with_the_child_winning() {
        let child = inherit(
            r#"<project><properties>
                 <shared>child</shared>
                 <only.child>x</only.child>
               </properties></project>"#,
            r#"<project><properties>
                 <shared>parent</shared>
                 <only.parent>y</only.parent>
                 <project.directory>should-not-inherit</project.directory>
               </properties></project>"#,
        );
        assert_eq!(child.properties.get("shared").unwrap(), "child");
        assert_eq!(child.properties.get("only.child").unwrap(), "x");
        assert_eq!(child.properties.get("only.parent").unwrap(), "y");
        assert!(!child.properties.contains_key("project.directory"));
    }

    #[test]
    fn dependencies_merge_by_key_with_the_child_replacing_wholesale() {
        let child = inherit(
            r#"<project><dependencies>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId></dependency>
               </dependencies></project>"#,
            r#"<project><dependencies>
                 <dependency>
                   <groupId>g</groupId><artifactId>a</artifactId>
                   <version>1.0</version><scope>test</scope>
                 </dependency>
                 <dependency><groupId>g</groupId><artifactId>b</artifactId><version>2.0</version></dependency>
               </dependencies></project>"#,
        );
        assert_eq!(child.dependencies.len(), 2);
        // Child order first, and the child's element wins entirely: it does NOT
        // pick up the parent's version or scope.
        assert_eq!(child.dependencies[0].artifact_id, "a");
        assert_eq!(child.dependencies[0].version, None);
        assert_eq!(child.dependencies[0].scope, None);
        assert_eq!(child.dependencies[1].artifact_id, "b");
    }

    #[test]
    fn dependency_key_separates_type_and_classifier() {
        let child = inherit(
            r#"<project><dependencies>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId></dependency>
               </dependencies></project>"#,
            r#"<project><dependencies>
                 <dependency>
                   <groupId>g</groupId><artifactId>a</artifactId>
                   <version>1.0</version><type>test-jar</type>
                 </dependency>
               </dependencies></project>"#,
        );
        // Different type means a different key, so both survive.
        assert_eq!(child.dependencies.len(), 2);
    }

    #[test]
    fn dependency_management_inherits_the_same_way() {
        let child = inherit(
            r#"<project/>"#,
            r#"<project><dependencyManagement><dependencies>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></dependency>
               </dependencies></dependencyManagement></project>"#,
        );
        assert_eq!(child.dependency_management.len(), 1);
        assert_eq!(
            child.dependency_management[0].version.as_deref(),
            Some("1.0")
        );
    }

    #[test]
    fn repositories_keep_the_child_first() {
        let child = inherit(
            r#"<project><repositories>
                 <repository><id>own</id><url>https://own</url></repository>
               </repositories></project>"#,
            r#"<project><repositories>
                 <repository><id>own</id><url>https://parent-override</url></repository>
                 <repository><id>central</id><url>https://central</url></repository>
               </repositories></project>"#,
        );
        assert_eq!(child.repositories.len(), 2);
        assert_eq!(child.repositories[0].id.as_deref(), Some("own"));
        // Same id is never partially merged; the child's declaration stands.
        assert_eq!(child.repositories[0].url.as_deref(), Some("https://own"));
        assert_eq!(child.repositories[1].id.as_deref(), Some("central"));
    }

    #[test]
    fn build_directories_fill_gaps() {
        let child = inherit(
            r#"<project><build><finalName>mine</finalName></build></project>"#,
            r#"<project><build>
                 <directory>/t</directory><finalName>theirs</finalName>
               </build></project>"#,
        );
        let build = child.build.as_ref().unwrap();
        assert_eq!(build.directory.as_deref(), Some("/t"));
        assert_eq!(build.final_name.as_deref(), Some("mine"));
    }

    #[test]
    fn plugin_order_follows_the_parent() {
        let child = inherit(
            r#"<project><build><plugins>
                 <plugin><artifactId>child-only</artifactId></plugin>
                 <plugin><artifactId>shared</artifactId><version>2.0</version></plugin>
               </plugins></build></project>"#,
            r#"<project><build><plugins>
                 <plugin><artifactId>shared</artifactId><version>1.0</version></plugin>
                 <plugin><artifactId>parent-only</artifactId><version>3.0</version></plugin>
               </plugins></build></project>"#,
        );
        let plugins = &child.build.as_ref().unwrap().plugins;
        let ids: Vec<&str> = plugins
            .iter()
            .map(|p| p.artifact_id.as_deref().unwrap_or(""))
            .collect();
        // Parent order dominates; the child-only plugin keeps its position ahead
        // of the key it preceded.
        assert_eq!(ids, vec!["child-only", "shared", "parent-only"]);
        // The child's version wins on the shared plugin.
        assert_eq!(plugins[1].version.as_deref(), Some("2.0"));
    }

    #[test]
    fn non_inherited_plugin_is_dropped() {
        let child = inherit(
            r#"<project/>"#,
            r#"<project><build><plugins>
                 <plugin><artifactId>private</artifactId><inherited>false</inherited></plugin>
                 <plugin><artifactId>public</artifactId></plugin>
               </plugins></build></project>"#,
        );
        let plugins = &child.build.as_ref().unwrap().plugins;
        let ids: Vec<&str> = plugins
            .iter()
            .map(|p| p.artifact_id.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(ids, vec!["public"]);
    }

    #[test]
    fn non_inherited_plugin_with_executions_still_contributes() {
        // The `|| !executions.isEmpty()` escape hatch: the plugin is a candidate
        // even with inherited=false, so its opted-in executions can survive.
        let child = inherit(
            r#"<project/>"#,
            r#"<project><build><plugins>
                 <plugin>
                   <artifactId>private</artifactId><inherited>false</inherited>
                   <executions><execution>
                     <id>run</id><inherited>true</inherited><goals><goal>go</goal></goals>
                   </execution></executions>
                 </plugin>
               </plugins></build></project>"#,
        );
        let plugins = &child.build.as_ref().unwrap().plugins;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].executions.len(), 1);
        assert_eq!(plugins[0].executions[0].id_or_default(), "run");
        // The container's own `inherited` is not carried over.
        assert_eq!(plugins[0].inherited, None);
    }

    #[test]
    fn execution_not_inherited_is_dropped() {
        let child = inherit(
            r#"<project/>"#,
            r#"<project><build><plugins><plugin>
                 <artifactId>p</artifactId>
                 <executions>
                   <execution><id>keep</id></execution>
                   <execution><id>drop</id><inherited>false</inherited></execution>
                 </executions>
               </plugin></plugins></build></project>"#,
        );
        let executions = &child.build.as_ref().unwrap().plugins[0].executions;
        let ids: Vec<&str> = executions
            .iter()
            .map(PluginExecution::id_or_default)
            .collect();
        assert_eq!(ids, vec!["keep"]);
    }

    #[test]
    fn relocation_is_never_inherited() {
        let child = inherit(
            r#"<project/>"#,
            r#"<project><distributionManagement>
                 <relocation><groupId>moved</groupId></relocation>
                 <status>deployed</status>
               </distributionManagement></project>"#,
        );
        let management = child.distribution_management.as_ref().unwrap();
        assert!(management.relocation.is_none());
        assert_eq!(management.status.as_deref(), Some("deployed"));
    }

    #[test]
    fn url_gets_the_child_path_appended() {
        let mut child = model_of(r#"<project><artifactId>child</artifactId></project>"#);
        let parent = model_of(
            r#"<project>
                 <url>http://acme.com/proj/</url>
                 <modules><module>sub/child</module></modules>
               </project>"#,
        );
        assemble_inheritance(&mut child, &parent, None);
        // The trailing slash survives, and `sub` comes from the module entry.
        assert_eq!(
            child.url.as_deref(),
            Some("http://acme.com/proj/sub/child/")
        );
    }

    #[test]
    fn url_child_path_can_be_overridden_by_a_property() {
        let mut child = model_of(
            r#"<project>
                 <artifactId>child</artifactId>
                 <properties><project.directory>modules/child</project.directory></properties>
               </project>"#,
        );
        let parent = model_of(
            r#"<project>
                 <url>http://acme.com/proj/</url>
                 <modules><module>sub/child</module></modules>
               </project>"#,
        );
        assemble_inheritance(&mut child, &parent, None);
        assert_eq!(
            child.url.as_deref(),
            Some("http://acme.com/proj/sub/modules/child/")
        );
    }

    #[test]
    fn url_without_nesting_appends_only_the_child() {
        let mut child = model_of(r#"<project><artifactId>child</artifactId></project>"#);
        let parent = model_of(
            r#"<project>
                 <url>http://acme.com/proj</url>
                 <modules><module>child</module></modules>
               </project>"#,
        );
        assemble_inheritance(&mut child, &parent, None);
        assert_eq!(child.url.as_deref(), Some("http://acme.com/proj/child"));
    }

    #[test]
    fn declared_url_is_left_alone() {
        let child = inherit(
            r#"<project><artifactId>child</artifactId><url>http://own</url></project>"#,
            r#"<project><url>http://acme.com/proj/</url></project>"#,
        );
        assert_eq!(child.url.as_deref(), Some("http://own"));
    }

    // ------------------------------------------------------- profile injection

    fn inject(model_xml: &str, profile_index: usize) -> Model {
        let mut model = model_of(model_xml);
        let profile = model.profiles[profile_index].clone();
        inject_profile(&mut model, &profile);
        model
    }

    #[test]
    fn profile_properties_win() {
        let model = inject(
            r#"<project>
                 <properties><flag>model</flag></properties>
                 <profiles><profile><id>p</id>
                   <properties><flag>profile</flag><extra>x</extra></properties>
                 </profile></profiles>
               </project>"#,
            0,
        );
        assert_eq!(model.properties.get("flag").unwrap(), "profile");
        assert_eq!(model.properties.get("extra").unwrap(), "x");
    }

    #[test]
    fn profile_dependencies_replace_in_place_and_append() {
        let model = inject(
            r#"<project>
                 <dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></dependency>
                 </dependencies>
                 <profiles><profile><id>p</id><dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId><version>2.0</version></dependency>
                   <dependency><groupId>g</groupId><artifactId>b</artifactId><version>3.0</version></dependency>
                 </dependencies></profile></profiles>
               </project>"#,
            0,
        );
        assert_eq!(model.dependencies.len(), 2);
        // Replaced in place, keeping position.
        assert_eq!(model.dependencies[0].artifact_id, "a");
        assert_eq!(model.dependencies[0].version.as_deref(), Some("2.0"));
        assert_eq!(model.dependencies[1].artifact_id, "b");
    }

    #[test]
    fn profile_repositories_come_first() {
        let model = inject(
            r#"<project>
                 <repositories><repository><id>model</id><url>https://m</url></repository></repositories>
                 <profiles><profile><id>p</id><repositories>
                   <repository><id>profile</id><url>https://p</url></repository>
                 </repositories></profile></profiles>
               </project>"#,
            0,
        );
        // The order flips: a profile's repositories are consulted first.
        let ids: Vec<&str> = model
            .repositories
            .iter()
            .map(|r| r.id.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(ids, vec!["profile", "model"]);
    }

    #[test]
    fn profile_plugins_keep_model_order() {
        let model = inject(
            r#"<project>
                 <build><plugins>
                   <plugin><artifactId>shared</artifactId><version>1.0</version></plugin>
                 </plugins></build>
                 <profiles><profile><id>p</id><build><plugins>
                   <plugin><artifactId>shared</artifactId><version>2.0</version></plugin>
                   <plugin><artifactId>extra</artifactId><version>3.0</version></plugin>
                 </plugins></build></profile></profiles>
               </project>"#,
            0,
        );
        let plugins = &model.build.as_ref().unwrap().plugins;
        let ids: Vec<&str> = plugins
            .iter()
            .map(|p| p.artifact_id.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(ids, vec!["shared", "extra"]);
        assert_eq!(plugins[0].version.as_deref(), Some("2.0"));
    }

    #[test]
    fn profile_modules_are_unioned() {
        let model = inject(
            r#"<project>
                 <modules><module>a</module></modules>
                 <profiles><profile><id>p</id>
                   <modules><module>a</module><module>b</module></modules>
                 </profile></profiles>
               </project>"#,
            0,
        );
        assert_eq!(model.modules, vec!["a", "b"]);
    }

    #[test]
    fn profile_dependency_management_merges() {
        let model = inject(
            r#"<project>
                 <profiles><profile><id>p</id><dependencyManagement><dependencies>
                   <dependency>
                     <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
                     <scope>import</scope><type>pom</type>
                   </dependency>
                 </dependencies></dependencyManagement></profile></profiles>
               </project>"#,
            0,
        );
        assert_eq!(model.dependency_management.len(), 1);
        assert_eq!(model.dependency_management[0].scope, Some(Scope::Import));
    }
}
