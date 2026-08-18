//! Lifecycle-bindings injection.
//!
//! Ported from `DefaultLifecycleBindingsInjector` and the half of
//! `DefaultLifecyclePluginAnalyzer` that turns a packaging's phase mapping into
//! `<plugin>` declarations.
//!
//! These plugins are in no POM. Maven synthesizes them from `<packaging>` and
//! merges them into the effective model, which is why `mvn help:effective-pom` on
//! a POM with no `<build>` at all still lists maven-compiler-plugin,
//! maven-surefire-plugin and five others. jv needs them for `jv sync`: a synced
//! `~/.m2` has to contain everything the offline `mvn` will ask for, and the first
//! thing it asks for is the plugins bound to the lifecycle.
//!
//! The bindings are data, not behaviour, so they are vendored rather than
//! transcribed — see the header of `resources/default-bindings-3.9.9.xml`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use jv_model::{Build, Model, Plugin, PluginExecution};
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::merge::merge_plugin;
use crate::problem::Problem;

/// The `default` lifecycle's bindings, one mapping per packaging.
///
/// Embedded verbatim from `maven-core-3.9.9.jar`
/// (`META-INF/plexus/default-bindings.xml`, Apache-2.0), for the same reason the
/// super POM is: the plugin *versions* in it change every Maven release, so a
/// transcription would be a second place to get 3.9.9 wrong. Regenerate with
/// `unzip -p $MAVEN_HOME/lib/maven-core-3.9.9.jar META-INF/plexus/default-bindings.xml`.
const DEFAULT_BINDINGS: &str = include_str!("../resources/default-bindings-3.9.9.xml");

/// The `clean` lifecycle's binding, which is packaging-independent.
///
/// This one does not live in `default-bindings.xml` but in the `Lifecycle`
/// component's `<default-phases>` in `META-INF/plexus/components.xml` of the same
/// jar — verify with
/// `unzip -p $MAVEN_HOME/lib/maven-core-3.9.9.jar META-INF/plexus/components.xml`.
/// That file is a thousand lines of unrelated component descriptors, so vendoring
/// it whole to reach three lines would hide the data rather than document it.
const CLEAN_BINDINGS: &[(&str, &str)] = &[(
    "clean",
    "org.apache.maven.plugins:maven-clean-plugin:3.2.0:clean",
)];

/// The `site` lifecycle's bindings, from the same `components.xml` as
/// [`CLEAN_BINDINGS`] and likewise the same for every packaging.
const SITE_BINDINGS: &[(&str, &str)] = &[
    (
        "site",
        "org.apache.maven.plugins:maven-site-plugin:3.12.1:site",
    ),
    (
        "site-deploy",
        "org.apache.maven.plugins:maven-site-plugin:3.12.1:deploy",
    ),
];

/// The plugins Maven 3.9.9 binds to `packaging`, or `None` for a packaging it
/// does not ship a mapping for.
///
/// The order is Maven's: `clean`, then the `default` lifecycle in phase order,
/// then `site` — upstream sorts its lifecycles by id before walking them, and
/// those three ids happen to sort that way. A plugin appears once, at its first
/// phase, carrying one execution per goal bound to it.
pub fn lifecycle_plugins(packaging: &str) -> Option<&'static [Plugin]> {
    bindings().get(packaging).map(Vec::as_slice)
}

/// Injects the plugins bound to the model's `<packaging>` into its `<build>`.
///
/// Runs after `<pluginManagement>` injection, which is what makes the
/// management fallback below the only path by which a managed version can reach
/// a lifecycle plugin.
pub fn inject_lifecycle_bindings(model: &mut Model, source: &str, problems: &mut Vec<Problem>) {
    let packaging = model.packaging_or_default();
    let Some(lifecycle) = lifecycle_plugins(packaging) else {
        // Maven calls this an error, because by the time it looks the packaging's
        // build extension has been loaded and a still-unknown packaging really is
        // a defect. jv does not load extensions, so for jv it is the known gap
        // showing through, and reporting an error would blame the POM for it.
        problems.push(Problem::warning(
            source,
            format!(
                "no lifecycle bindings are known for <packaging>{packaging}</packaging>; \
                 it comes from a build extension, which jv does not load, so the plugins \
                 that packaging binds are missing from the effective model"
            ),
        ));
        return;
    };
    let build = model.build.get_or_insert_with(Build::default);
    merge_lifecycle_plugins(build, lifecycle);
}

/// Merges the lifecycle's plugins into a build section.
///
/// The direction is the whole point: the POM's plugins are the *target* and
/// dominate, so a project that pins maven-surefire-plugin keeps its own version
/// and gains only the executions it did not declare. Lifecycle plugins the POM
/// says nothing about are appended, after everything the POM declared.
fn merge_lifecycle_plugins(build: &mut Build, lifecycle: &[Plugin]) {
    // Cloned up front because the loop needs both lists at once, and the managed
    // list is four entries in the common case.
    let managed = build.plugin_management.clone();

    for candidate in lifecycle {
        let key = candidate.key();
        if let Some(declared) = build.plugins.iter_mut().find(|plugin| plugin.key() == key) {
            // Reusing the inheritance-shaped merge is safe because the
            // synthesized plugins never set `<inherited>`, so the gates it
            // applies to a recessive parent plugin can never fire here.
            merge_plugin(declared, candidate, false);
            continue;
        }

        // An appended plugin is rebuilt on top of its `<pluginManagement>` entry
        // with the *managed* declaration as the base, so management wins over the
        // lifecycle. This is not a shortcut for the injection that already ran:
        // that pass could only reach plugins already in `<plugins>`, and this one
        // was not there yet, so a POM pinning maven-compiler-plugin through
        // management has no other way to be heard.
        let appended = match managed.iter().find(|entry| entry.key() == key) {
            Some(entry) => {
                let mut base = entry.clone();
                merge_plugin(&mut base, candidate, false);
                base
            }
            None => candidate.clone(),
        };
        build.plugins.push(appended);
    }
}

/// The parsed bindings, keyed by packaging.
fn bindings() -> &'static BTreeMap<String, Vec<Plugin>> {
    static PARSED: OnceLock<BTreeMap<String, Vec<Plugin>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        parse_default_bindings(DEFAULT_BINDINGS)
            .into_iter()
            .map(|(packaging, phases)| (packaging, plugins_for(&phases)))
            .collect()
    })
}

/// Reads the plexus component descriptor into `(packaging, [(phase, goals)])`.
///
/// A tag-directed scan rather than a document walk: the shape being looked for is
/// two elements deep in a file that is otherwise plexus wiring jv has no use for,
/// and the file is vendored, so anything unexpected in it is a mistake in this
/// repository rather than input to tolerate.
fn parse_default_bindings(xml: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut mappings: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut packaging: Option<String> = None;
    let mut lifecycle_id: Option<String> = None;
    let mut phases: Vec<(String, String)> = Vec::new();
    let mut in_phases = false;
    let mut open: Option<String> = None;

    loop {
        match reader
            .read_event()
            .expect("the embedded bindings must parse")
        {
            Event::Start(tag) => {
                let name = String::from_utf8_lossy(tag.local_name().as_ref()).into_owned();
                // Inside `<phases>` the element *name* is the phase, so the tag
                // cannot be matched against a fixed vocabulary.
                if name == "phases" {
                    in_phases = true;
                } else {
                    open = Some(name);
                }
            }
            Event::Text(text) => {
                let Some(name) = open.as_deref() else {
                    continue;
                };
                let value = text
                    .unescape()
                    .expect("the embedded bindings must parse")
                    .trim()
                    .to_owned();
                if in_phases {
                    phases.push((name.to_owned(), value));
                } else if name == "role-hint" {
                    packaging = Some(value);
                } else if name == "id" {
                    lifecycle_id = Some(value);
                }
            }
            Event::End(tag) => {
                let name = tag.local_name();
                if name.as_ref() == b"phases" {
                    in_phases = false;
                    // 3.9.9 maps only the `default` lifecycle per packaging;
                    // `clean` and `site` come from the constants above, so a
                    // component that mapped them here would double-bind them.
                    let collected = std::mem::take(&mut phases);
                    let wanted = lifecycle_id.as_deref() == Some("default");
                    if let Some(packaging) = packaging.clone().filter(|_| wanted) {
                        mappings.push((packaging, collected));
                    }
                } else if name.as_ref() == b"component" {
                    packaging = None;
                    lifecycle_id = None;
                }
                open = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }

    mappings
}

/// Expands a packaging's `default` phases into plugins, framed by the two
/// lifecycles every packaging shares.
///
/// The three lifecycles are walked in id order — `clean`, `default`, `site` —
/// because upstream sorts them by id before walking, precisely so that the plugin
/// list does not depend on the order plexus happened to register them in.
fn plugins_for(default_phases: &[(String, String)]) -> Vec<Plugin> {
    let clean = CLEAN_BINDINGS.to_vec();
    let default: Vec<(&str, &str)> = default_phases
        .iter()
        .map(|(phase, goals)| (phase.as_str(), goals.as_str()))
        .collect();
    let site = SITE_BINDINGS.to_vec();

    let mut plugins = Vec::new();
    for lifecycle in [clean, default, site] {
        for (phase, goals) in in_hash_map_order(&lifecycle) {
            bind(&mut plugins, phase, goals);
        }
    }
    plugins
}

/// Java `HashMap`'s initial table size, which a lifecycle mapping never outgrows:
/// the largest is `maven-plugin` with nine phases, under the twelve that would
/// force a resize.
const JAVA_HASH_MAP_BUCKETS: u32 = 16;

/// Reorders a lifecycle's phases the way Maven visits them.
///
/// This is nobody's design decision, including jv's. Plexus deserializes the
/// `<phases>` element into a plain `java.util.HashMap` and
/// `DefaultLifecyclePluginAnalyzer` walks its `entrySet`, so the order plugins
/// appear in — and the order of two executions on one plugin — is Java's string
/// hash showing through every effective POM Maven prints. Reproducing it is what
/// lets jv's `<build><plugins>` be compared to Maven's as a list rather than as a
/// set, which is the only comparison that would catch a phase bound to the wrong
/// plugin.
///
/// Verified against `mvn help:effective-pom` on all seven packagings; see
/// `tests/mvn_oracle.rs` for the standing check.
fn in_hash_map_order<'a>(phases: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut ordered = phases.to_vec();
    // Stable, because within one bucket Java keeps insertion — that is, document
    // — order.
    ordered.sort_by_key(|(phase, _)| java_bucket(phase));
    ordered
}

/// The bucket `HashMap` would put a key in: `String.hashCode`, spread, masked.
fn java_bucket(key: &str) -> u32 {
    let mut hash: u32 = 0;
    // `String.hashCode` is defined over UTF-16 code units. Phase names are ASCII,
    // but spelling it faithfully costs nothing and removes the caveat.
    for unit in key.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(unit));
    }
    (hash ^ (hash >> 16)) & (JAVA_HASH_MAP_BUCKETS - 1)
}

/// Binds every goal one phase names. A phase may name several, comma-separated
/// and usually wrapped across lines.
fn bind(plugins: &mut Vec<Plugin>, phase: &str, goals: &str) {
    for spec in goals.split(',') {
        let spec = spec.trim();
        if !spec.is_empty() {
            bind_goal(plugins, phase, spec);
        }
    }
}

/// Binds one `groupId:artifactId:version:goal`.
fn bind_goal(plugins: &mut Vec<Plugin>, phase: &str, spec: &str) {
    let mut fields = spec.split(':');
    let (Some(group_id), Some(artifact_id), Some(version), Some(goal), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        panic!("`{spec}` is not a groupId:artifactId:version:goal binding");
    };

    let key = format!("{group_id}:{artifact_id}");
    let existing = plugins
        .iter_mut()
        .find(|plugin: &&mut Plugin| plugin.key() == key);

    // Upstream also stamps a `priority` on each execution so that goals bound to
    // one phase keep their declared order at build time. jv does not run builds
    // and does not model the field, and it never reaches the effective POM.
    match existing {
        Some(plugin) => {
            let execution = execution(&plugin.executions, phase, goal);
            plugin.executions.push(execution);
        }
        None => plugins.push(Plugin {
            group_id: Some(group_id.to_owned()),
            artifact_id: Some(artifact_id.to_owned()),
            version: Some(version.to_owned()),
            executions: vec![execution(&[], phase, goal)],
            ..Plugin::default()
        }),
    }
}

/// Builds the execution for one goal, with the id Maven gives it.
///
/// `default-<goal>` is the id users see in build output and write
/// `<execution><id>default-compile</id>` against to reconfigure a bound goal, so
/// it is part of the contract rather than a label. The numeric suffix is
/// upstream's collision escape; no 3.9.9 packaging binds the same goal twice.
fn execution(existing: &[PluginExecution], phase: &str, goal: &str) -> PluginExecution {
    let base = format!("default-{goal}");
    let mut id = base.clone();
    let mut suffix = 0;
    while existing.iter().any(|other| other.id_or_default() == id) {
        suffix += 1;
        id = format!("{base}-{suffix}");
    }
    PluginExecution {
        id: Some(id),
        phase: Some(phase.to_owned()),
        goals: vec![goal.to_owned()],
        inherited: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::parse_pom;

    /// A binding set as `artifactId:version` pairs, which is what a `jv sync`
    /// download list is.
    fn coordinates(packaging: &str) -> Vec<String> {
        lifecycle_plugins(packaging)
            .expect("a known packaging")
            .iter()
            .map(|plugin| {
                format!(
                    "{}:{}",
                    plugin.artifact_id.as_deref().unwrap_or_default(),
                    plugin.version.as_deref().unwrap_or_default()
                )
            })
            .collect()
    }

    /// One plugin's executions as `id@phase[goals]`.
    fn executions(packaging: &str, artifact_id: &str) -> Vec<String> {
        lifecycle_plugins(packaging)
            .expect("a known packaging")
            .iter()
            .find(|plugin| plugin.artifact_id.as_deref() == Some(artifact_id))
            .unwrap_or_else(|| panic!("{packaging} binds no {artifact_id}"))
            .executions
            .iter()
            .map(|execution| {
                format!(
                    "{}@{}[{}]",
                    execution.id_or_default(),
                    execution.phase.as_deref().unwrap_or_default(),
                    execution.goals.join(",")
                )
            })
            .collect()
    }

    fn model_of(xml: &str) -> Model {
        parse_pom(xml).expect("parse").model
    }

    /// The build plugins as `artifactId:version`.
    fn build_plugins(model: &Model) -> Vec<String> {
        model
            .build
            .as_ref()
            .expect("a build section")
            .plugins
            .iter()
            .map(|plugin| {
                format!(
                    "{}:{}",
                    plugin.artifact_id.as_deref().unwrap_or_default(),
                    plugin.version.as_deref().unwrap_or_default()
                )
            })
            .collect()
    }

    fn inject(model: &mut Model) -> Vec<Problem> {
        let mut problems = Vec::new();
        inject_lifecycle_bindings(model, "test", &mut problems);
        problems
    }

    #[test]
    fn every_packaging_maven_3_9_9_ships_is_mapped() {
        for packaging in ["pom", "jar", "ejb", "maven-plugin", "war", "ear", "rar"] {
            assert!(
                lifecycle_plugins(packaging).is_some(),
                "{packaging} has no bindings"
            );
        }
        assert_eq!(bindings().len(), 7);
    }

    #[test]
    fn a_pom_packaging_binds_only_install_and_deploy() {
        // Nothing is built, so the default lifecycle contributes only the two
        // phases that move the POM itself. clean and site are not packaging
        // specific and come along regardless.
        assert_eq!(
            coordinates("pom"),
            [
                "maven-clean-plugin:3.2.0",
                "maven-install-plugin:3.1.2",
                "maven-deploy-plugin:3.1.2",
                "maven-site-plugin:3.12.1",
            ]
        );
    }

    #[test]
    fn a_jar_packaging_binds_the_whole_compile_test_package_chain() {
        assert_eq!(
            coordinates("jar"),
            [
                "maven-clean-plugin:3.2.0",
                "maven-resources-plugin:3.3.1",
                // maven-jar-plugin ahead of the compiler that produces what it
                // packages: the order is a hash bucket walk, not a phase order.
                "maven-jar-plugin:3.4.1",
                "maven-compiler-plugin:3.13.0",
                "maven-surefire-plugin:3.2.5",
                "maven-install-plugin:3.1.2",
                "maven-deploy-plugin:3.1.2",
                "maven-site-plugin:3.12.1",
            ]
        );
    }

    #[test]
    fn the_binding_order_is_javas_hash_map_order() {
        // Pinned because it is unguessable and because a plugin list in a
        // different order is a diff against `mvn help:effective-pom`. If this
        // ever breaks, check `in_hash_map_order` against the oracle before
        // changing the expectation.
        assert_eq!(java_bucket("process-test-resources"), 0);
        assert_eq!(java_bucket("package"), 3);
        assert_eq!(java_bucket("process-resources"), 9);
        assert_eq!(java_bucket("deploy"), 15);
        // `compile` and `test` collide, so the chain keeps them in the order the
        // XML declared them — which is why the sort has to be a stable one.
        assert_eq!(java_bucket("compile"), java_bucket("test"));
        assert_eq!(
            in_hash_map_order(&[
                ("deploy", ""),
                ("compile", ""),
                ("test", ""),
                ("process-test-resources", ""),
            ]),
            [
                ("process-test-resources", ""),
                ("compile", ""),
                ("test", ""),
                ("deploy", ""),
            ]
        );
    }

    #[test]
    fn the_packaging_decides_only_which_artifact_plugin_is_bound() {
        for (packaging, packager) in [
            ("war", "maven-war-plugin:3.4.0"),
            ("ejb", "maven-ejb-plugin:3.2.1"),
            ("rar", "maven-rar-plugin:3.0.0"),
        ] {
            let bound = coordinates(packaging);
            assert!(
                bound.contains(&packager.to_owned()),
                "{packaging}: {bound:?}"
            );
            // Everything else is the jar chain.
            assert!(bound.contains(&"maven-surefire-plugin:3.2.5".to_owned()));
        }
        // ear is the exception: it neither compiles nor tests.
        let ear = coordinates("ear");
        assert!(ear.contains(&"maven-ear-plugin:3.3.0".to_owned()));
        assert!(!ear.contains(&"maven-compiler-plugin:3.13.0".to_owned()));
    }

    #[test]
    fn clean_and_site_are_bound_whatever_the_packaging() {
        for packaging in ["pom", "jar", "ejb", "maven-plugin", "war", "ear", "rar"] {
            assert_eq!(
                executions(packaging, "maven-clean-plugin"),
                ["default-clean@clean[clean]"]
            );
            assert_eq!(
                executions(packaging, "maven-site-plugin"),
                [
                    "default-site@site[site]",
                    "default-deploy@site-deploy[deploy]"
                ]
            );
        }
    }

    #[test]
    fn a_plugin_bound_to_several_phases_gets_one_execution_each() {
        assert_eq!(
            executions("jar", "maven-resources-plugin"),
            [
                // The test phase's execution first, again because of the bucket
                // walk rather than anything about the phases.
                "default-testResources@process-test-resources[testResources]",
                "default-resources@process-resources[resources]",
            ]
        );
    }

    #[test]
    fn a_phase_binding_two_goals_binds_both() {
        // `package` for maven-plugin packaging is the only comma-separated entry
        // in the file, and its second goal lands on a plugin already bound at an
        // earlier phase — so it must extend that plugin rather than duplicate it.
        assert_eq!(
            executions("maven-plugin", "maven-plugin-plugin"),
            [
                "default-addPluginArtifactMetadata@package[addPluginArtifactMetadata]",
                "default-descriptor@process-classes[descriptor]",
            ]
        );
        assert_eq!(
            coordinates("maven-plugin")
                .iter()
                .filter(|entry| entry.starts_with("maven-plugin-plugin"))
                .count(),
            1
        );
    }

    #[test]
    fn a_pom_that_declares_nothing_gains_a_build_section() {
        let mut model = model_of(r#"<project><artifactId>a</artifactId></project>"#);
        assert!(model.build.is_none());
        assert!(inject(&mut model).is_empty());
        assert_eq!(build_plugins(&model).len(), 8);
    }

    #[test]
    fn the_poms_own_version_beats_the_lifecycles() {
        let mut model = model_of(
            r#"<project><build><plugins>
                 <plugin><artifactId>maven-surefire-plugin</artifactId><version>3.5.0</version></plugin>
               </plugins></build></project>"#,
        );
        inject(&mut model);
        let plugins = build_plugins(&model);
        assert_eq!(plugins[0], "maven-surefire-plugin:3.5.0");
        assert!(!plugins.contains(&"maven-surefire-plugin:3.2.5".to_owned()));
    }

    #[test]
    fn lifecycle_plugins_are_appended_after_the_poms_own() {
        let mut model = model_of(
            r#"<project><build><plugins>
                 <plugin><artifactId>maven-shade-plugin</artifactId><version>3.5.3</version></plugin>
                 <plugin><artifactId>maven-jar-plugin</artifactId></plugin>
               </plugins></build></project>"#,
        );
        inject(&mut model);
        let plugins = build_plugins(&model);
        // Declaration order is preserved, including for the one the lifecycle
        // also binds, which only gains a version.
        assert_eq!(plugins[0], "maven-shade-plugin:3.5.3");
        assert_eq!(plugins[1], "maven-jar-plugin:3.4.1");
        assert_eq!(plugins[2], "maven-clean-plugin:3.2.0");
        assert_eq!(plugins.len(), 9);
    }

    #[test]
    fn an_execution_the_pom_declares_merges_by_id_with_the_poms_values_winning() {
        let mut model = model_of(
            r#"<project><build><plugins>
                 <plugin><artifactId>maven-compiler-plugin</artifactId>
                   <executions>
                     <execution><id>default-compile</id><phase>process-sources</phase></execution>
                     <execution><id>extra</id><goals><goal>help</goal></goals></execution>
                   </executions>
                 </plugin>
               </plugins></build></project>"#,
        );
        inject(&mut model);
        let compiler = &model.build.as_ref().expect("build").plugins[0];
        let ids: Vec<&str> = compiler
            .executions
            .iter()
            .map(PluginExecution::id_or_default)
            .collect();
        // The lifecycle's ordering wins; the POM's own execution is appended.
        assert_eq!(ids, ["default-compile", "default-testCompile", "extra"]);
        // The rebound phase is the POM's, and the goal it never named survives.
        assert_eq!(
            compiler.executions[0].phase.as_deref(),
            Some("process-sources")
        );
        assert_eq!(compiler.executions[0].goals, ["compile"]);
    }

    #[test]
    fn goals_union_target_first_without_duplicates() {
        let mut model = model_of(
            r#"<project><build><plugins>
                 <plugin><artifactId>maven-compiler-plugin</artifactId>
                   <executions>
                     <execution><id>default-compile</id>
                       <goals><goal>help</goal><goal>compile</goal></goals>
                     </execution>
                   </executions>
                 </plugin>
               </plugins></build></project>"#,
        );
        inject(&mut model);
        let compiler = &model.build.as_ref().expect("build").plugins[0];
        // `compile` is bound by the lifecycle too and must not appear twice, and
        // must not be reordered ahead of the POM's own goal.
        assert_eq!(compiler.executions[0].goals, ["help", "compile"]);
    }

    #[test]
    fn plugin_management_beats_the_lifecycle_for_an_appended_plugin() {
        let mut model = model_of(
            r#"<project><build>
                 <pluginManagement><plugins>
                   <plugin><artifactId>maven-compiler-plugin</artifactId><version>3.11.0</version></plugin>
                 </plugins></pluginManagement>
               </build></project>"#,
        );
        inject(&mut model);
        let plugins = build_plugins(&model);
        // The plugin was not in <plugins> when pluginManagement injection ran, so
        // this is the pass that has to honour the pin.
        assert!(plugins.contains(&"maven-compiler-plugin:3.11.0".to_owned()));
        assert!(!plugins.contains(&"maven-compiler-plugin:3.13.0".to_owned()));
        // Position is still the lifecycle's, not the management list's.
        assert_eq!(plugins[3], "maven-compiler-plugin:3.11.0");
    }

    #[test]
    fn a_managed_plugin_no_lifecycle_binds_stays_out_of_the_build() {
        let mut model = model_of(
            r#"<project><build>
                 <pluginManagement><plugins>
                   <plugin><artifactId>maven-antrun-plugin</artifactId><version>3.1.0</version></plugin>
                 </plugins></pluginManagement>
               </build></project>"#,
        );
        inject(&mut model);
        assert!(
            !build_plugins(&model)
                .iter()
                .any(|plugin| plugin.starts_with("maven-antrun-plugin"))
        );
    }

    #[test]
    fn an_unknown_packaging_injects_nothing_and_warns() {
        let mut model = model_of(
            r#"<project><packaging>bundle</packaging>
                 <build><plugins>
                   <plugin><artifactId>maven-bundle-plugin</artifactId><version>5.1.9</version></plugin>
                 </plugins></build>
               </project>"#,
        );
        let problems = inject(&mut model);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].severity, crate::problem::Severity::Warning);
        // What the POM declared is untouched; only the extension's bindings are
        // missing.
        assert_eq!(build_plugins(&model), ["maven-bundle-plugin:5.1.9"]);
    }
}
