//! `<dependencyManagement>`, BOM imports, and normalization.
//!
//! Ported from `DefaultDependencyManagementImporter`,
//! `DefaultDependencyManagementInjector`, `DefaultPluginManagementInjector` and
//! `DefaultModelNormalizer`.

use std::collections::{HashMap, HashSet};

use jv_model::{Dependency, ManagementKey, Model, Plugin, Scope};

use crate::merge::merge_plugin;
use crate::problem::Problem;

/// Removes every `<type>pom</type><scope>import</scope>` entry from the model's
/// `<dependencyManagement>` and returns them in declaration order.
///
/// Imports never appear in the effective model, so taking them out is part of
/// processing them. Entries missing any coordinate are reported and dropped:
/// upstream does no version inference for a BOM, so an import without a version
/// is simply unusable.
pub fn extract_bom_imports(
    model: &mut Model,
    source: &str,
    problems: &mut Vec<Problem>,
) -> Vec<Dependency> {
    let mut imports = Vec::new();
    let mut retained = Vec::with_capacity(model.dependency_management.len());

    for entry in std::mem::take(&mut model.dependency_management) {
        let is_import = entry.type_.as_deref() == Some("pom") && entry.scope == Some(Scope::Import);
        if !is_import {
            retained.push(entry);
            continue;
        }
        let version = entry.version.as_deref().unwrap_or("");
        if entry.group_id.is_empty() || entry.artifact_id.is_empty() || version.is_empty() {
            problems.push(Problem::error(
                source,
                format!(
                    "imported POM {}:{} must declare a groupId, artifactId and version",
                    entry.group_id, entry.artifact_id
                ),
            ));
            continue;
        }
        imports.push(entry);
    }

    model.dependency_management = retained;
    imports
}

/// Folds imported management sections into the model's own.
///
/// `imported` holds one list per BOM, in the order the BOMs were declared. The
/// precedence is strict and worth stating plainly: whatever the model already
/// manages — locally or by inheritance — wins over every BOM, and between two
/// BOMs managing the same key the first-declared one wins.
pub fn merge_imported_management(model: &mut Model, imported: &[Vec<Dependency>]) {
    // A set, not a list. A Spring Boot project's effective management runs to
    // over a thousand entries across nested BOMs, and a linear `contains` over
    // owned keys made this quadratic in that number.
    let mut seen: HashSet<ManagementKey> = model
        .dependency_management
        .iter()
        .map(Dependency::management_key)
        .collect();

    for bom in imported {
        for entry in bom {
            let key = entry.management_key();
            if !seen.insert(key) {
                continue;
            }
            model.dependency_management.push(entry.clone());
        }
    }
}

/// Applies `<dependencyManagement>` to the declared dependencies.
///
/// Management only fills in what a declaration left out. The exceptions are the
/// interesting part: `optional` is never injected at all, and `exclusions` are
/// all-or-nothing — a declaration with even one exclusion of its own keeps only
/// its own.
pub fn inject_dependency_management(model: &mut Model) {
    if model.dependency_management.is_empty() {
        return;
    }
    // Indexed once rather than scanned per dependency, and by index rather than
    // by clone: `dependency_management` is the big list here, and cloning it
    // wholesale to satisfy the borrow checker copied a thousand entries on every
    // model build.
    let mut at: HashMap<ManagementKey, usize> =
        HashMap::with_capacity(model.dependency_management.len());
    for (index, entry) in model.dependency_management.iter().enumerate() {
        // First rule wins, matching the scan this replaces.
        at.entry(entry.management_key()).or_insert(index);
    }

    let managed = std::mem::take(&mut model.dependency_management);
    for dependency in &mut model.dependencies {
        let Some(entry) = at
            .get(&dependency.management_key())
            .map(|index| &managed[*index])
        else {
            continue;
        };
        if dependency.version.is_none() {
            dependency.version = entry.version.clone();
        }
        if dependency.scope.is_none() {
            dependency.scope = entry.scope;
        }
        if dependency.system_path.is_none() {
            dependency.system_path = entry.system_path.clone();
        }
        // `optional` is deliberately never injected.
        if dependency.exclusions.is_empty() {
            dependency.exclusions = entry.exclusions.clone();
        }
    }
    model.dependency_management = managed;
}

/// Applies `<pluginManagement>` to the declared plugins.
///
/// Matched by `groupId:artifactId`, filling gaps only. Managed entries with no
/// matching `<plugin>` add nothing to the build.
pub fn inject_plugin_management(model: &mut Model) {
    let Some(build) = &mut model.build else {
        return;
    };
    if build.plugin_management.is_empty() {
        return;
    }
    let managed = build.plugin_management.clone();
    for plugin in &mut build.plugins {
        let key = plugin.key();
        if let Some(entry) = managed.iter().find(|candidate| candidate.key() == key) {
            merge_plugin(plugin, entry, false);
        }
    }
}

/// Collapses duplicate declarations.
///
/// Both rules keep the *first* occurrence's position while letting the *last*
/// declaration's content win. That is deliberate Maven 2.x bug-compatibility, not
/// an accident, and a POM that declares the same dependency twice depends on it.
pub fn merge_duplicates(model: &mut Model) {
    // Dependencies: last declaration wins, first position kept.
    let mut keys: Vec<ManagementKey> = Vec::new();
    let mut merged: Vec<Dependency> = Vec::new();
    for dependency in std::mem::take(&mut model.dependencies) {
        let key = dependency.management_key();
        match keys.iter().position(|existing| *existing == key) {
            Some(index) => merged[index] = dependency,
            None => {
                keys.push(key);
                merged.push(dependency);
            }
        }
    }
    model.dependencies = merged;

    // Build plugins: the later declaration is the target and wins per field,
    // with the first filling gaps.
    if let Some(build) = &mut model.build {
        let mut plugin_keys: Vec<String> = Vec::new();
        let mut plugins: Vec<Plugin> = Vec::new();
        for plugin in std::mem::take(&mut build.plugins) {
            let key = plugin.key();
            match plugin_keys.iter().position(|existing| *existing == key) {
                Some(index) => {
                    let mut later = plugin;
                    merge_plugin(&mut later, &plugins[index], false);
                    plugins[index] = later;
                }
                None => {
                    plugin_keys.push(key);
                    plugins.push(plugin);
                }
            }
        }
        build.plugins = plugins;
    }
}

/// Materializes the `compile` scope default.
///
/// This runs *after* `<dependencyManagement>` injection, which is why the default
/// cannot simply live in the model: a managed `scope` has to be able to fill a
/// declaration's gap before the gap is filled with `compile`.
pub fn inject_default_values(model: &mut Model) {
    for dependency in &mut model.dependencies {
        if dependency.scope.is_none() {
            dependency.scope = Some(Scope::Compile);
        }
    }
    if let Some(build) = &mut model.build {
        // `<build><plugins>` only. `injectDefaultValues` walks the model's
        // dependencies and the *build* plugins' dependencies, and leaves
        // `<pluginManagement>` alone — checked against 3.9.9, whose effective POM
        // shows a scope beside a build plugin's dependency and none beside the
        // same dependency under pluginManagement.
        for plugin in &mut build.plugins {
            for dependency in &mut plugin.dependencies {
                if dependency.scope.is_none() {
                    dependency.scope = Some(Scope::Compile);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::{Exclusion, parse_pom};

    fn model_of(xml: &str) -> Model {
        parse_pom(xml).expect("parse").model
    }

    #[test]
    fn imports_are_extracted_and_removed() {
        let mut model = model_of(
            r#"<project><dependencyManagement><dependencies>
                 <dependency>
                   <groupId>org.springframework.boot</groupId>
                   <artifactId>spring-boot-dependencies</artifactId>
                   <version>3.2.0</version><type>pom</type><scope>import</scope>
                 </dependency>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></dependency>
               </dependencies></dependencyManagement></project>"#,
        );
        let mut problems = Vec::new();
        let imports = extract_bom_imports(&mut model, "test", &mut problems);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].artifact_id, "spring-boot-dependencies");
        // The import itself never appears in the effective model.
        assert_eq!(model.dependency_management.len(), 1);
        assert_eq!(model.dependency_management[0].artifact_id, "a");
        assert!(problems.is_empty());
    }

    #[test]
    fn import_without_a_version_is_rejected() {
        let mut model = model_of(
            r#"<project><dependencyManagement><dependencies>
                 <dependency>
                   <groupId>g</groupId><artifactId>bom</artifactId>
                   <type>pom</type><scope>import</scope>
                 </dependency>
               </dependencies></dependencyManagement></project>"#,
        );
        let mut problems = Vec::new();
        let imports = extract_bom_imports(&mut model, "test", &mut problems);
        // No version inference for a BOM, so it is dropped with an error.
        assert!(imports.is_empty());
        assert!(model.dependency_management.is_empty());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_pom_dependency_without_import_scope_is_not_an_import() {
        let mut model = model_of(
            r#"<project><dependencyManagement><dependencies>
                 <dependency>
                   <groupId>g</groupId><artifactId>a</artifactId>
                   <version>1.0</version><type>pom</type>
                 </dependency>
               </dependencies></dependencyManagement></project>"#,
        );
        let mut problems = Vec::new();
        assert!(extract_bom_imports(&mut model, "test", &mut problems).is_empty());
        assert_eq!(model.dependency_management.len(), 1);
    }

    fn managed(group: &str, artifact: &str, version: &str) -> Dependency {
        Dependency::new(group, artifact, version)
    }

    #[test]
    fn local_management_beats_every_bom() {
        let mut model = model_of(
            r#"<project><dependencyManagement><dependencies>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId><version>local</version></dependency>
               </dependencies></dependencyManagement></project>"#,
        );
        merge_imported_management(&mut model, &[vec![managed("g", "a", "from-bom")]]);
        assert_eq!(model.dependency_management.len(), 1);
        assert_eq!(
            model.dependency_management[0].version.as_deref(),
            Some("local")
        );
    }

    #[test]
    fn the_first_bom_wins_between_boms() {
        let mut model = model_of(r#"<project/>"#);
        merge_imported_management(
            &mut model,
            &[
                vec![managed("g", "a", "first")],
                vec![managed("g", "a", "second"), managed("g", "b", "only")],
            ],
        );
        assert_eq!(model.dependency_management.len(), 2);
        assert_eq!(
            model.dependency_management[0].version.as_deref(),
            Some("first")
        );
        assert_eq!(model.dependency_management[1].artifact_id, "b");
    }

    #[test]
    fn management_fills_gaps_only() {
        let mut model = model_of(
            r#"<project>
                 <dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId></dependency>
                   <dependency>
                     <groupId>g</groupId><artifactId>b</artifactId>
                     <version>own</version><scope>test</scope>
                   </dependency>
                 </dependencies>
                 <dependencyManagement><dependencies>
                   <dependency>
                     <groupId>g</groupId><artifactId>a</artifactId>
                     <version>managed</version><scope>runtime</scope>
                   </dependency>
                   <dependency>
                     <groupId>g</groupId><artifactId>b</artifactId>
                     <version>managed</version><scope>provided</scope>
                   </dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        );
        inject_dependency_management(&mut model);
        assert_eq!(model.dependencies[0].version.as_deref(), Some("managed"));
        assert_eq!(model.dependencies[0].scope, Some(Scope::Runtime));
        // An explicit value is never overridden.
        assert_eq!(model.dependencies[1].version.as_deref(), Some("own"));
        assert_eq!(model.dependencies[1].scope, Some(Scope::Test));
    }

    #[test]
    fn management_never_injects_optional() {
        let mut model = model_of(
            r#"<project>
                 <dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId></dependency>
                 </dependencies>
                 <dependencyManagement><dependencies>
                   <dependency>
                     <groupId>g</groupId><artifactId>a</artifactId>
                     <version>1.0</version><optional>true</optional>
                   </dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        );
        inject_dependency_management(&mut model);
        assert_eq!(model.dependencies[0].version.as_deref(), Some("1.0"));
        assert_eq!(model.dependencies[0].optional, None);
        assert!(!model.dependencies[0].is_optional());
    }

    #[test]
    fn managed_exclusions_are_all_or_nothing() {
        let mut model = model_of(
            r#"<project>
                 <dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId></dependency>
                   <dependency>
                     <groupId>g</groupId><artifactId>b</artifactId>
                     <exclusions><exclusion><groupId>own</groupId><artifactId>x</artifactId></exclusion></exclusions>
                   </dependency>
                 </dependencies>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
                     <exclusions><exclusion><groupId>m</groupId><artifactId>y</artifactId></exclusion></exclusions>
                   </dependency>
                   <dependency><groupId>g</groupId><artifactId>b</artifactId><version>1.0</version>
                     <exclusions><exclusion><groupId>m</groupId><artifactId>y</artifactId></exclusions>
                   </dependency>
                 </dependencies></dependencyManagement>
               </project>"#
                .replace("</artifactId></exclusions>", "</artifactId></exclusion></exclusions>")
                .as_str(),
        );
        inject_dependency_management(&mut model);
        // The declaration with none gets the managed list.
        assert_eq!(
            model.dependencies[0].exclusions,
            vec![Exclusion::new("m", "y")]
        );
        // The declaration with one keeps only its own; there is no union.
        assert_eq!(
            model.dependencies[1].exclusions,
            vec![Exclusion::new("own", "x")]
        );
    }

    #[test]
    fn management_matches_on_type_and_classifier() {
        let mut model = model_of(
            r#"<project>
                 <dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId><type>test-jar</type></dependency>
                 </dependencies>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        );
        inject_dependency_management(&mut model);
        // The managed entry is a plain jar, so it does not apply.
        assert_eq!(model.dependencies[0].version, None);
    }

    #[test]
    fn duplicate_dependencies_keep_the_first_position_and_the_last_content() {
        let mut model = model_of(
            r#"<project><dependencies>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></dependency>
                 <dependency><groupId>g</groupId><artifactId>b</artifactId><version>2.0</version></dependency>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId><version>3.0</version></dependency>
               </dependencies></project>"#,
        );
        merge_duplicates(&mut model);
        assert_eq!(model.dependencies.len(), 2);
        assert_eq!(model.dependencies[0].artifact_id, "a");
        // Position of the first, content of the last.
        assert_eq!(model.dependencies[0].version.as_deref(), Some("3.0"));
        assert_eq!(model.dependencies[1].artifact_id, "b");
    }

    #[test]
    fn duplicate_plugins_merge_with_the_later_winning() {
        let mut model = model_of(
            r#"<project><build><plugins>
                 <plugin><artifactId>p</artifactId><version>1.0</version></plugin>
                 <plugin><artifactId>q</artifactId><version>9.0</version></plugin>
                 <plugin><artifactId>p</artifactId><version>2.0</version></plugin>
               </plugins></build></project>"#,
        );
        merge_duplicates(&mut model);
        let plugins = &model.build.as_ref().unwrap().plugins;
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].artifact_id.as_deref(), Some("p"));
        assert_eq!(plugins[0].version.as_deref(), Some("2.0"));
    }

    #[test]
    fn plugin_management_dependencies_keep_no_default_scope() {
        // Maven's `injectDefaultValues` walks `model.dependencies` and
        // `build.plugins[*].dependencies`, and stops there. Its effective POM
        // shows `<scope>compile</scope>` beside a build plugin's dependency and
        // nothing beside the same dependency under `<pluginManagement>`.
        let mut model = model_of(
            r#"<project><build>
                 <pluginManagement><plugins><plugin><artifactId>managed</artifactId>
                   <dependencies><dependency><groupId>d</groupId>
                     <artifactId>a</artifactId></dependency></dependencies>
                 </plugin></plugins></pluginManagement>
                 <plugins><plugin><artifactId>built</artifactId>
                   <dependencies><dependency><groupId>d</groupId>
                     <artifactId>b</artifactId></dependency></dependencies>
                 </plugin></plugins>
               </build></project>"#,
        );
        inject_default_values(&mut model);
        let build = model.build.as_ref().expect("a build section");
        assert_eq!(build.plugins[0].dependencies[0].scope, Some(Scope::Compile));
        assert_eq!(build.plugin_management[0].dependencies[0].scope, None);
    }

    #[test]
    fn plugin_management_fills_gaps() {
        let mut model = model_of(
            r#"<project><build>
                 <pluginManagement><plugins>
                   <plugin><artifactId>maven-compiler-plugin</artifactId><version>3.13.0</version></plugin>
                   <plugin><artifactId>unused</artifactId><version>1.0</version></plugin>
                 </plugins></pluginManagement>
                 <plugins>
                   <plugin><artifactId>maven-compiler-plugin</artifactId></plugin>
                 </plugins>
               </build></project>"#,
        );
        inject_plugin_management(&mut model);
        let build = model.build.as_ref().unwrap();
        assert_eq!(build.plugins.len(), 1);
        assert_eq!(build.plugins[0].version.as_deref(), Some("3.13.0"));
    }

    #[test]
    fn default_scope_is_materialized_after_management() {
        let mut model = model_of(
            r#"<project><dependencies>
                 <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></dependency>
               </dependencies>
               <dependencyManagement><dependencies>
                 <dependency><groupId>g</groupId><artifactId>b</artifactId><version>1.0</version></dependency>
               </dependencies></dependencyManagement></project>"#,
        );
        inject_default_values(&mut model);
        assert_eq!(model.dependencies[0].scope, Some(Scope::Compile));
        // Management entries do not get the default.
        assert_eq!(model.dependency_management[0].scope, None);
    }
}
