//! End-to-end effective-POM construction.
//!
//! The unit tests in each module check one transformation. These check that the
//! transformations compose in the right order, which is where an effective-POM
//! implementation actually goes wrong: nearly every step depends on an earlier
//! one having already run, so a plausible-looking pipeline in the wrong order
//! produces plausible-looking wrong versions.

use jv_model::{Scope, parse_pom};
use jv_model_builder::{BuildContext, MapModelSource, ModelBuilder, SourcedModel};

/// Builds an effective model from a child POM plus a set of resolvable POMs.
fn build(child: &str, registered: &[(&str, &str, &str, &str)]) -> jv_model_builder::EffectiveModel {
    build_with(child, registered, BuildContext::empty())
}

fn build_with(
    child: &str,
    registered: &[(&str, &str, &str, &str)],
    context: BuildContext,
) -> jv_model_builder::EffectiveModel {
    let mut source = MapModelSource::new();
    for (group, artifact, version, pom) in registered {
        source.insert(group, artifact, version, *pom);
    }
    let model = parse_pom(child).expect("child parses").model;
    ModelBuilder::new(&source, context)
        .build(SourcedModel::new(model, "test/pom.xml"))
        .expect("build succeeds")
}

/// The dependency list as `dependency:tree` would spell it, for compact asserts.
fn dependency_lines(built: &jv_model_builder::EffectiveModel) -> Vec<String> {
    built
        .model
        .dependencies
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn super_pom_supplies_central_and_build_directories() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
           </project>"#,
        &[],
    );
    // Every POM inherits central from the super POM; without it nothing resolves.
    let central = built
        .model
        .repositories
        .iter()
        .find(|repository| repository.id.as_deref() == Some("central"))
        .expect("central repository");
    assert_eq!(
        central.url.as_deref(),
        Some("https://repo.maven.apache.org/maven2")
    );
    // Snapshots are disabled on central.
    assert_eq!(
        central.snapshots.as_ref().and_then(|policy| policy.enabled),
        Some(false)
    );
    assert!(
        built
            .model
            .plugin_repositories
            .iter()
            .any(|repository| repository.id.as_deref() == Some("central"))
    );
    // The super POM's build directories arrive interpolated.
    let build_section = built.model.build.as_ref().expect("build");
    assert_eq!(build_section.final_name.as_deref(), Some("a-1.0"));
    assert!(built.lineage.last().is_some_and(|last| last == "super POM"));
}

#[test]
fn coordinates_and_properties_come_down_the_chain() {
    let built = build(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>p</artifactId><version>1.0</version></parent>
             <artifactId>child</artifactId>
           </project>"#,
        &[(
            "g",
            "p",
            "1.0",
            r#"<project>
                 <groupId>g</groupId><artifactId>p</artifactId><version>1.0</version>
                 <properties><shared>from-parent</shared></properties>
               </project>"#,
        )],
    );
    assert_eq!(built.model.group_id.as_deref(), Some("g"));
    assert_eq!(built.model.version.as_deref(), Some("1.0"));
    assert_eq!(built.model.artifact_id.as_deref(), Some("child"));
    assert_eq!(built.model.properties.get("shared").unwrap(), "from-parent");
}

#[test]
fn a_grandparent_property_reaches_a_managed_version() {
    // Three levels, with the version travelling from the grandparent's property
    // through the parent's management into the child's declaration. Every step of
    // the pipeline has to have run in order for this to resolve.
    let built = build(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>mid</artifactId><version>1.0</version></parent>
             <artifactId>child</artifactId>
             <dependencies>
               <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[
            (
                "g",
                "mid",
                "1.0",
                r#"<project>
                     <parent><groupId>g</groupId><artifactId>root</artifactId><version>1.0</version></parent>
                     <artifactId>mid</artifactId>
                     <dependencyManagement><dependencies>
                       <dependency>
                         <groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId>
                         <version>${slf4j.version}</version><scope>runtime</scope>
                       </dependency>
                     </dependencies></dependencyManagement>
                   </project>"#,
            ),
            (
                "g",
                "root",
                "1.0",
                r#"<project>
                     <groupId>g</groupId><artifactId>root</artifactId><version>1.0</version>
                     <properties><slf4j.version>2.0.9</slf4j.version></properties>
                   </project>"#,
            ),
        ],
    );
    assert_eq!(
        dependency_lines(&built),
        vec!["org.slf4j:slf4j-api:jar:2.0.9:runtime"]
    );
    assert_eq!(built.lineage.len(), 4); // child, mid, root, super POM
}

#[test]
fn a_declared_version_beats_management() {
    let built = build(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>p</artifactId><version>1.0</version></parent>
             <artifactId>child</artifactId>
             <dependencies>
               <dependency><groupId>x</groupId><artifactId>y</artifactId><version>9.9</version></dependency>
             </dependencies>
           </project>"#,
        &[(
            "g",
            "p",
            "1.0",
            r#"<project>
                 <groupId>g</groupId><artifactId>p</artifactId><version>1.0</version>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>x</groupId><artifactId>y</artifactId><version>1.0</version></dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        )],
    );
    assert_eq!(dependency_lines(&built), vec!["x:y:jar:9.9:compile"]);
}

#[test]
fn bom_import_supplies_versions() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>app</artifactId><version>1.0</version>
             <dependencyManagement><dependencies>
               <dependency>
                 <groupId>com.vendor</groupId><artifactId>bom</artifactId><version>3.2.0</version>
                 <type>pom</type><scope>import</scope>
               </dependency>
             </dependencies></dependencyManagement>
             <dependencies>
               <dependency><groupId>com.vendor</groupId><artifactId>lib</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[(
            "com.vendor",
            "bom",
            "3.2.0",
            r#"<project>
                 <groupId>com.vendor</groupId><artifactId>bom</artifactId><version>3.2.0</version>
                 <packaging>pom</packaging>
                 <properties><lib.version>7.1</lib.version></properties>
                 <dependencyManagement><dependencies>
                   <dependency>
                     <groupId>com.vendor</groupId><artifactId>lib</artifactId>
                     <version>${lib.version}</version>
                   </dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        )],
    );
    // The BOM's own property resolved while building the BOM.
    assert_eq!(
        dependency_lines(&built),
        vec!["com.vendor:lib:jar:7.1:compile"]
    );
    // The import entry itself is gone from the effective model.
    assert!(
        !built
            .model
            .dependency_management
            .iter()
            .any(|entry| entry.artifact_id == "bom")
    );
}

#[test]
fn a_bom_property_is_not_visible_to_the_importer() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>app</artifactId><version>1.0</version>
             <dependencyManagement><dependencies>
               <dependency>
                 <groupId>v</groupId><artifactId>bom</artifactId><version>1.0</version>
                 <type>pom</type><scope>import</scope>
               </dependency>
             </dependencies></dependencyManagement>
             <dependencies>
               <dependency><groupId>v</groupId><artifactId>other</artifactId><version>${bom.only}</version></dependency>
             </dependencies>
           </project>"#,
        &[(
            "v",
            "bom",
            "1.0",
            r#"<project>
                 <groupId>v</groupId><artifactId>bom</artifactId><version>1.0</version>
                 <properties><bom.only>should-not-leak</bom.only></properties>
               </project>"#,
        )],
    );
    // An import brings dependencyManagement and nothing else.
    assert!(!built.model.properties.contains_key("bom.only"));
    assert_eq!(
        built.model.dependencies[0].version.as_deref(),
        Some("${bom.only}")
    );
}

#[test]
fn local_management_wins_over_a_bom() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>app</artifactId><version>1.0</version>
             <dependencyManagement><dependencies>
               <dependency><groupId>v</groupId><artifactId>lib</artifactId><version>pinned</version></dependency>
               <dependency>
                 <groupId>v</groupId><artifactId>bom</artifactId><version>1.0</version>
                 <type>pom</type><scope>import</scope>
               </dependency>
             </dependencies></dependencyManagement>
             <dependencies>
               <dependency><groupId>v</groupId><artifactId>lib</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[(
            "v",
            "bom",
            "1.0",
            r#"<project>
                 <groupId>v</groupId><artifactId>bom</artifactId><version>1.0</version>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>v</groupId><artifactId>lib</artifactId><version>from-bom</version></dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        )],
    );
    assert_eq!(dependency_lines(&built), vec!["v:lib:jar:pinned:compile"]);
}

#[test]
fn the_first_of_two_boms_wins() {
    let bom = |name: &str, version: &str| {
        format!(
            r#"<project>
                 <groupId>v</groupId><artifactId>{name}</artifactId><version>1.0</version>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>v</groupId><artifactId>lib</artifactId><version>{version}</version></dependency>
                 </dependencies></dependencyManagement>
               </project>"#
        )
    };
    let first = bom("first", "from-first");
    let second = bom("second", "from-second");
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>app</artifactId><version>1.0</version>
             <dependencyManagement><dependencies>
               <dependency><groupId>v</groupId><artifactId>first</artifactId><version>1.0</version>
                 <type>pom</type><scope>import</scope></dependency>
               <dependency><groupId>v</groupId><artifactId>second</artifactId><version>1.0</version>
                 <type>pom</type><scope>import</scope></dependency>
             </dependencies></dependencyManagement>
             <dependencies>
               <dependency><groupId>v</groupId><artifactId>lib</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[
            ("v", "first", "1.0", first.as_str()),
            ("v", "second", "1.0", second.as_str()),
        ],
    );
    assert_eq!(
        dependency_lines(&built),
        vec!["v:lib:jar:from-first:compile"]
    );
}

#[test]
fn a_bom_may_import_another_bom() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>app</artifactId><version>1.0</version>
             <dependencyManagement><dependencies>
               <dependency><groupId>v</groupId><artifactId>outer</artifactId><version>1.0</version>
                 <type>pom</type><scope>import</scope></dependency>
             </dependencies></dependencyManagement>
             <dependencies>
               <dependency><groupId>v</groupId><artifactId>lib</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[
            (
                "v",
                "outer",
                "1.0",
                r#"<project>
                     <groupId>v</groupId><artifactId>outer</artifactId><version>1.0</version>
                     <dependencyManagement><dependencies>
                       <dependency><groupId>v</groupId><artifactId>inner</artifactId><version>1.0</version>
                         <type>pom</type><scope>import</scope></dependency>
                     </dependencies></dependencyManagement>
                   </project>"#,
            ),
            (
                "v",
                "inner",
                "1.0",
                r#"<project>
                     <groupId>v</groupId><artifactId>inner</artifactId><version>1.0</version>
                     <dependencyManagement><dependencies>
                       <dependency><groupId>v</groupId><artifactId>lib</artifactId><version>nested</version></dependency>
                     </dependencies></dependencyManagement>
                   </project>"#,
            ),
        ],
    );
    assert_eq!(dependency_lines(&built), vec!["v:lib:jar:nested:compile"]);
}

#[test]
fn a_bom_cycle_is_reported_and_survivable() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>app</artifactId><version>1.0</version>
             <dependencyManagement><dependencies>
               <dependency><groupId>v</groupId><artifactId>a</artifactId><version>1.0</version>
                 <type>pom</type><scope>import</scope></dependency>
             </dependencies></dependencyManagement>
           </project>"#,
        &[
            (
                "v",
                "a",
                "1.0",
                r#"<project>
                     <groupId>v</groupId><artifactId>a</artifactId><version>1.0</version>
                     <dependencyManagement><dependencies>
                       <dependency><groupId>v</groupId><artifactId>b</artifactId><version>1.0</version>
                         <type>pom</type><scope>import</scope></dependency>
                     </dependencies></dependencyManagement>
                   </project>"#,
            ),
            (
                "v",
                "b",
                "1.0",
                r#"<project>
                     <groupId>v</groupId><artifactId>b</artifactId><version>1.0</version>
                     <dependencyManagement><dependencies>
                       <dependency><groupId>v</groupId><artifactId>a</artifactId><version>1.0</version>
                         <type>pom</type><scope>import</scope></dependency>
                     </dependencies></dependencyManagement>
                   </project>"#,
            ),
        ],
    );
    // Reported, not fatal: Maven carries on with what it has.
    assert!(
        built
            .errors()
            .any(|problem| problem.message.contains("cycle")),
        "problems were {:?}",
        built.problems
    );
}

#[test]
fn an_unresolvable_bom_is_reported_and_survivable() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>app</artifactId><version>1.0</version>
             <dependencyManagement><dependencies>
               <dependency><groupId>v</groupId><artifactId>absent</artifactId><version>1.0</version>
                 <type>pom</type><scope>import</scope></dependency>
             </dependencies></dependencyManagement>
           </project>"#,
        &[],
    );
    assert!(built.errors().count() > 0);
}

#[test]
fn profiles_contribute_dependencies_and_are_injected_before_inheritance() {
    // The parent's active profile adds management that the child then uses. This
    // only works if profile injection happens before inheritance, as in Maven 3.
    let built = build_with(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>p</artifactId><version>1.0</version></parent>
             <artifactId>child</artifactId>
             <dependencies>
               <dependency><groupId>x</groupId><artifactId>y</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[(
            "g",
            "p",
            "1.0",
            r#"<project>
                 <groupId>g</groupId><artifactId>p</artifactId><version>1.0</version>
                 <profiles><profile>
                   <id>ci</id>
                   <activation><property><name>ci</name></property></activation>
                   <dependencyManagement><dependencies>
                     <dependency><groupId>x</groupId><artifactId>y</artifactId><version>ci-only</version></dependency>
                   </dependencies></dependencyManagement>
                 </profile></profiles>
               </project>"#,
        )],
        BuildContext::empty().with_system_property("ci", "true"),
    );
    assert_eq!(dependency_lines(&built), vec!["x:y:jar:ci-only:compile"]);
    assert!(built.active_profiles.contains(&"ci".to_owned()));
}

#[test]
fn an_inactive_profile_contributes_nothing() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
             <profiles><profile>
               <id>ci</id>
               <activation><property><name>ci</name></property></activation>
               <dependencies>
                 <dependency><groupId>x</groupId><artifactId>y</artifactId><version>1.0</version></dependency>
               </dependencies>
             </profile></profiles>
           </project>"#,
        &[],
    );
    assert!(built.model.dependencies.is_empty());
    assert!(built.active_profiles.is_empty());
}

#[test]
fn ci_friendly_versions_resolve_across_the_chain() {
    let built = build(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>p</artifactId><version>${revision}</version></parent>
             <artifactId>child</artifactId>
             <properties><revision>2.1.0</revision></properties>
           </project>"#,
        &[(
            "g",
            "p",
            "2.1.0",
            r#"<project>
                 <groupId>g</groupId><artifactId>p</artifactId><version>${revision}</version>
                 <properties><revision>2.1.0</revision></properties>
               </project>"#,
        )],
    );
    // The parent was looked up at the resolved version, and the child inherited it.
    assert_eq!(built.model.version.as_deref(), Some("2.1.0"));
}

#[test]
fn a_command_line_property_overrides_a_ci_friendly_version() {
    let built = build_with(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>${revision}</version>
             <properties><revision>1.0-SNAPSHOT</revision></properties>
           </project>"#,
        &[],
        BuildContext::empty().with_user_property("revision", "4.5.6"),
    );
    assert_eq!(built.model.version.as_deref(), Some("4.5.6"));
}

#[test]
fn duplicate_declarations_collapse_before_management() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
             <dependencies>
               <dependency><groupId>x</groupId><artifactId>y</artifactId><version>1.0</version></dependency>
               <dependency><groupId>x</groupId><artifactId>z</artifactId><version>2.0</version></dependency>
               <dependency><groupId>x</groupId><artifactId>y</artifactId><version>3.0</version></dependency>
             </dependencies>
           </project>"#,
        &[],
    );
    // First position, last content.
    assert_eq!(
        dependency_lines(&built),
        vec!["x:y:jar:3.0:compile", "x:z:jar:2.0:compile"]
    );
}

#[test]
fn a_parent_cycle_is_a_hard_error() {
    let mut source = MapModelSource::new();
    source.insert(
        "g",
        "a",
        "1.0",
        r#"<project>
             <parent><groupId>g</groupId><artifactId>b</artifactId><version>1.0</version></parent>
             <artifactId>a</artifactId>
           </project>"#,
    );
    source.insert(
        "g",
        "b",
        "1.0",
        r#"<project>
             <parent><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></parent>
             <artifactId>b</artifactId>
           </project>"#,
    );
    let child = parse_pom(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></parent>
             <artifactId>start</artifactId>
           </project>"#,
    )
    .unwrap()
    .model;
    let error = ModelBuilder::new(&source, BuildContext::empty())
        .build(SourcedModel::new(child, "start/pom.xml"))
        .expect_err("a cycle must fail");
    assert!(error.to_string().contains("circular"), "got {error}");
}

#[test]
fn a_missing_parent_is_a_hard_error() {
    let child = parse_pom(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>absent</artifactId><version>1.0</version></parent>
             <artifactId>a</artifactId>
           </project>"#,
    )
    .unwrap()
    .model;
    let source = MapModelSource::new();
    let error = ModelBuilder::new(&source, BuildContext::empty())
        .build(SourcedModel::new(child, "a/pom.xml"))
        .expect_err("a missing parent must fail");
    assert!(error.to_string().contains("cannot read POM"), "got {error}");
}

#[test]
fn a_relative_path_parent_is_preferred_over_the_repository() {
    let mut source = MapModelSource::new();
    source.insert_at_path(
        "/work/pom.xml",
        r#"<project>
             <groupId>g</groupId><artifactId>p</artifactId><version>1.0</version>
             <properties><origin>disk</origin></properties>
           </project>"#,
    );
    source.insert(
        "g",
        "p",
        "1.0",
        r#"<project>
             <groupId>g</groupId><artifactId>p</artifactId><version>1.0</version>
             <properties><origin>repository</origin></properties>
           </project>"#,
    );
    let child = parse_pom(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>p</artifactId><version>1.0</version></parent>
             <artifactId>child</artifactId>
           </project>"#,
    )
    .unwrap()
    .model;
    let built = ModelBuilder::new(&source, BuildContext::empty())
        .build(SourcedModel::new(child, "child/pom.xml").with_basedir("/work/child"))
        .expect("build");
    assert_eq!(built.model.properties.get("origin").unwrap(), "disk");
}

#[test]
fn a_mismatched_relative_path_falls_back_to_the_repository() {
    let mut source = MapModelSource::new();
    source.insert_at_path(
        "/work/pom.xml",
        r#"<project>
             <groupId>g</groupId><artifactId>somebody-else</artifactId><version>1.0</version>
           </project>"#,
    );
    source.insert(
        "g",
        "p",
        "1.0",
        r#"<project>
             <groupId>g</groupId><artifactId>p</artifactId><version>1.0</version>
             <properties><origin>repository</origin></properties>
           </project>"#,
    );
    let child = parse_pom(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>p</artifactId><version>1.0</version></parent>
             <artifactId>child</artifactId>
           </project>"#,
    )
    .unwrap()
    .model;
    let built = ModelBuilder::new(&source, BuildContext::empty())
        .build(SourcedModel::new(child, "child/pom.xml").with_basedir("/work/child"))
        .expect("build");
    assert_eq!(built.model.properties.get("origin").unwrap(), "repository");
    assert!(
        built
            .problems
            .iter()
            .any(|problem| problem.message.contains("relativePath"))
    );
}

#[test]
fn settings_profiles_contribute_properties_and_repositories() {
    let settings = jv_model::parse_settings(
        r#"<settings><profiles><profile>
             <id>corp</id>
             <activation><activeByDefault>true</activeByDefault></activation>
             <properties><lib.version>corp-1.0</lib.version></properties>
             <repositories><repository>
               <id>corp</id><url>https://nexus.corp/public</url>
             </repository></repositories>
           </profile></profiles></settings>"#,
    )
    .expect("settings");

    let mut source = MapModelSource::new();
    source.insert("ignored", "ignored", "0", "<project/>");
    let child = parse_pom(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
             <dependencies>
               <dependency><groupId>x</groupId><artifactId>y</artifactId><version>${lib.version}</version></dependency>
             </dependencies>
           </project>"#,
    )
    .unwrap()
    .model;
    let built = ModelBuilder::new(&source, BuildContext::empty())
        .with_settings_profiles(&settings.profiles)
        .build(SourcedModel::new(child, "a/pom.xml"))
        .expect("build");

    assert_eq!(dependency_lines(&built), vec!["x:y:jar:corp-1.0:compile"]);
    // The profile's repository comes ahead of central.
    let ids: Vec<&str> = built
        .model
        .repositories
        .iter()
        .map(|repository| repository.id.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(ids.first(), Some(&"corp"));
    assert!(ids.contains(&"central"));
}

#[test]
fn a_settings_default_profile_is_not_suppressed_by_a_pom_profile() {
    let settings = jv_model::parse_settings(
        r#"<settings><profiles><profile>
             <id>always</id>
             <activation><activeByDefault>true</activeByDefault></activation>
             <properties><from.settings>yes</from.settings></properties>
           </profile></profiles></settings>"#,
    )
    .expect("settings");
    let source = MapModelSource::new();
    let child = parse_pom(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
             <profiles><profile>
               <id>pom-one</id>
               <activation><property><name>ci</name></property></activation>
             </profile></profiles>
           </project>"#,
    )
    .unwrap()
    .model;
    let built = ModelBuilder::new(
        &source,
        BuildContext::empty().with_system_property("ci", "1"),
    )
    .with_settings_profiles(&settings.profiles)
    .build(SourcedModel::new(child, "a/pom.xml"))
    .expect("build");
    assert!(built.active_profiles.contains(&"always".to_owned()));
    assert!(built.active_profiles.contains(&"pom-one".to_owned()));
    assert_eq!(built.model.properties.get("from.settings").unwrap(), "yes");
}

#[test]
fn plugin_management_and_inheritance_compose() {
    let built = build(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>p</artifactId><version>1.0</version></parent>
             <artifactId>child</artifactId>
             <build><plugins>
               <plugin><artifactId>maven-compiler-plugin</artifactId></plugin>
             </plugins></build>
           </project>"#,
        &[(
            "g",
            "p",
            "1.0",
            r#"<project>
                 <groupId>g</groupId><artifactId>p</artifactId><version>1.0</version>
                 <build><pluginManagement><plugins>
                   <plugin><artifactId>maven-compiler-plugin</artifactId><version>3.13.0</version></plugin>
                 </plugins></pluginManagement></build>
               </project>"#,
        )],
    );
    let plugins = &built.model.build.as_ref().unwrap().plugins;
    let compiler = plugins
        .iter()
        .find(|plugin| plugin.artifact_id.as_deref() == Some("maven-compiler-plugin"))
        .expect("compiler plugin");
    // The version came from the parent's pluginManagement.
    assert_eq!(compiler.version.as_deref(), Some("3.13.0"));
}

#[test]
fn the_super_poms_plugin_management_is_available() {
    // The 3.9 super POM pins a handful of plugin versions; a project declaring
    // one of them without a version must still get one.
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
             <build><plugins>
               <plugin><artifactId>maven-assembly-plugin</artifactId></plugin>
             </plugins></build>
           </project>"#,
        &[],
    );
    let plugins = &built.model.build.as_ref().unwrap().plugins;
    let assembly = plugins
        .iter()
        .find(|plugin| plugin.artifact_id.as_deref() == Some("maven-assembly-plugin"))
        .expect("assembly plugin");
    assert!(
        assembly.version.is_some(),
        "the super POM should have supplied a version"
    );
}

#[test]
fn lifecycle_bindings_stay_out_unless_asked_for() {
    // Resolving dependencies never reads <build><plugins>, so the callers that
    // only resolve must not start paying for a plugin list.
    let built = build(
        r#"<project><groupId>g</groupId><artifactId>a</artifactId><version>1.0</version></project>"#,
        &[],
    );
    assert!(
        built
            .model
            .build
            .as_ref()
            .is_none_or(|build| build.plugins.is_empty())
    );
}

#[test]
fn lifecycle_bindings_are_injected_after_plugin_management() {
    // The pin is only reachable by the lifecycle merge: pluginManagement
    // injection ran while <plugins> was still empty.
    let mut source = MapModelSource::new();
    source.insert(
        "g",
        "parent",
        "1.0",
        r#"<project>
             <groupId>g</groupId><artifactId>parent</artifactId><version>1.0</version>
             <build><pluginManagement><plugins>
               <plugin><artifactId>maven-surefire-plugin</artifactId><version>3.5.0</version></plugin>
             </plugins></pluginManagement></build>
           </project>"#,
    );
    let child = parse_pom(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
             <artifactId>a</artifactId>
           </project>"#,
    )
    .expect("child parses")
    .model;
    let built = ModelBuilder::new(&source, BuildContext::empty())
        .with_lifecycle_bindings(true)
        .build(SourcedModel::new(child, "test/pom.xml"))
        .expect("build succeeds");

    let plugins = &built.model.build.as_ref().expect("a build section").plugins;
    let surefire = plugins
        .iter()
        .find(|plugin| plugin.artifact_id.as_deref() == Some("maven-surefire-plugin"))
        .expect("surefire is bound to the jar lifecycle");
    assert_eq!(surefire.version.as_deref(), Some("3.5.0"));
    assert_eq!(surefire.executions[0].phase.as_deref(), Some("test"));
}

#[test]
fn scope_and_optional_survive_the_pipeline_intact() {
    let built = build(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1.0</version>
             <dependencies>
               <dependency>
                 <groupId>x</groupId><artifactId>y</artifactId><version>1.0</version>
                 <scope>test</scope><optional>true</optional>
                 <exclusions><exclusion><groupId>e</groupId><artifactId>*</artifactId></exclusion></exclusions>
               </dependency>
             </dependencies>
           </project>"#,
        &[],
    );
    let dependency = &built.model.dependencies[0];
    assert_eq!(dependency.scope, Some(Scope::Test));
    assert!(dependency.is_optional());
    assert_eq!(dependency.exclusions.len(), 1);
    assert!(dependency.exclusions[0].matches("e", "anything"));
}

// ---------------------------------------------------------------- activation
//
// Every case below was checked against a real Maven 3.9.9 before being written
// down. These are the activators whose rules are surprising enough that reading
// the source got them wrong at least once.

/// The property that nothing declares and everything can activate on.
#[test]
fn packaging_activates_a_profile_without_anyone_declaring_it() {
    // `getProfileActivationContext` seeds `packaging` into the user properties.
    // Without it this activator can never fire, which is how it read before —
    // silently, since an activator that never matches looks exactly like one
    // whose condition is false.
    let built = build_with(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1</version>
             <packaging>war</packaging>
             <profiles><profile><id>wars</id>
               <activation><property><name>packaging</name><value>war</value></property></activation>
               <properties><marker>yes</marker></properties>
             </profile></profiles>
           </project>"#,
        &[],
        BuildContext::empty(),
    );
    assert_eq!(built.active_profiles, ["wars"]);
    assert_eq!(
        built.model.properties.get("marker").map(String::as_str),
        Some("yes")
    );
}

#[test]
fn an_explicit_packaging_property_beats_the_seeded_one() {
    // `computeIfAbsent`, so `-Dpackaging=...` wins.
    let mut context = BuildContext::empty();
    context
        .user_properties
        .insert("packaging".to_owned(), "jar".to_owned());
    let built = build_with(
        r#"<project>
             <groupId>g</groupId><artifactId>a</artifactId><version>1</version>
             <packaging>war</packaging>
             <profiles><profile><id>wars</id>
               <activation><property><name>packaging</name><value>war</value></property></activation>
             </profile></profiles>
           </project>"#,
        &[],
        context,
    );
    assert!(built.active_profiles.is_empty());
}

#[test]
fn an_activation_value_reads_the_poms_property_before_the_command_lines() {
    // Verified: with `<flavour>frompom</flavour>` in the POM and
    // `-Dflavour=fromcli -Dmarker=fromcli`, Maven leaves this profile inactive,
    // because it interpolates the *value* against the POM's properties first.
    // That is the reverse of interpolation proper, where `-D` wins.
    let mut context = BuildContext::empty();
    context
        .user_properties
        .insert("flavour".to_owned(), "fromcli".to_owned());
    context
        .user_properties
        .insert("marker".to_owned(), "fromcli".to_owned());

    let pom = r#"<project>
         <groupId>g</groupId><artifactId>a</artifactId><version>1</version>
         <properties><flavour>frompom</flavour></properties>
         <profiles><profile><id>p</id>
           <activation><property><name>marker</name><value>${flavour}</value></property></activation>
         </profile></profiles>
       </project>"#;
    assert!(build_with(pom, &[], context).active_profiles.is_empty());

    // And it activates when the command line names what the POM says.
    let mut context = BuildContext::empty();
    context
        .user_properties
        .insert("flavour".to_owned(), "fromcli".to_owned());
    context
        .user_properties
        .insert("marker".to_owned(), "frompom".to_owned());
    assert_eq!(build_with(pom, &[], context).active_profiles, ["p"]);
}

// ------------------------------------------------- duplicates and basedir

#[test]
fn a_duplicated_managed_key_resolves_to_the_last_when_management_is_inherited() {
    // Checked against 3.9.9 both ways, because the rule is conditional and
    // reads like a bug either way you find it. Maven's generated merger seeds
    // its list with `mergeAll(target, sourceDominant=true)` — which collapses
    // the target's duplicates last-wins — but only when there is a source list
    // to merge, i.e. only when something was inherited.
    let built = build_with(
        r#"<project>
             <parent><groupId>g</groupId><artifactId>parent</artifactId><version>1</version></parent>
             <artifactId>child</artifactId>
             <dependencyManagement><dependencies>
               <dependency><groupId>d</groupId><artifactId>d</artifactId><version>FIRST</version></dependency>
               <dependency><groupId>d</groupId><artifactId>d</artifactId><version>SECOND</version></dependency>
             </dependencies></dependencyManagement>
             <dependencies>
               <dependency><groupId>d</groupId><artifactId>d</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[(
            "g",
            "parent",
            "1",
            r#"<project><groupId>g</groupId><artifactId>parent</artifactId><version>1</version>
                 <dependencyManagement><dependencies>
                   <dependency><groupId>other</groupId><artifactId>other</artifactId><version>9</version></dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        )],
        BuildContext::empty(),
    );
    assert_eq!(
        built.model.dependencies[0].version.as_deref(),
        Some("SECOND")
    );
}

#[test]
fn a_duplicated_managed_key_resolves_to_the_first_when_nothing_is_inherited() {
    // The other half of the same rule: with no parent management the merge never
    // runs, the duplicates are never collapsed, and injection finds the first.
    let built = build_with(
        r#"<project>
             <groupId>g</groupId><artifactId>solo</artifactId><version>1</version>
             <dependencyManagement><dependencies>
               <dependency><groupId>d</groupId><artifactId>d</artifactId><version>FIRST</version></dependency>
               <dependency><groupId>d</groupId><artifactId>d</artifactId><version>SECOND</version></dependency>
             </dependencies></dependencyManagement>
             <dependencies>
               <dependency><groupId>d</groupId><artifactId>d</artifactId></dependency>
             </dependencies>
           </project>"#,
        &[],
        BuildContext::empty(),
    );
    assert_eq!(
        built.model.dependencies[0].version.as_deref(),
        Some("FIRST")
    );
}

#[test]
fn a_parents_file_activation_looks_beside_the_child() {
    // `<file><exists>` in a *parent* resolves against the directory of the POM
    // being built, not the parent's own — Maven sets `projectDirectory` once,
    // from the request's POM file. Checked against 3.9.9: a marker beside the
    // parent leaves the profile inactive when the child is built.
    let workspace = tempfile::tempdir().expect("a temp dir");
    let child_dir = workspace.path().join("child");
    std::fs::create_dir_all(&child_dir).unwrap();
    std::fs::write(workspace.path().join("marker.txt"), b"beside the parent").unwrap();

    let parent = r#"<project><groupId>g</groupId><artifactId>parent</artifactId><version>1</version>
         <profiles><profile><id>has-marker</id>
           <activation><file><exists>marker.txt</exists></file></activation>
         </profile></profiles></project>"#;
    let child = r#"<project>
         <parent><groupId>g</groupId><artifactId>parent</artifactId><version>1</version></parent>
         <artifactId>child</artifactId></project>"#;

    // Registered *at a path*, so it is found through `<relativePath>` and
    // carries a basedir of its own — which is exactly the case the two rules
    // differ on. A parent looked up by coordinates has no directory, so the old
    // code fell back to the child's and looked right.
    let mut source = MapModelSource::new();
    source.insert_at_path(workspace.path().join("pom.xml"), parent);
    let model = parse_pom(child).expect("child parses").model;
    let built = ModelBuilder::new(&source, BuildContext::empty())
        .build(SourcedModel::new(model, "child/pom.xml").with_basedir(&child_dir))
        .expect("build succeeds");
    assert!(
        built.active_profiles.is_empty(),
        "the marker beside the parent should not activate while building the child"
    );

    // And it does activate once the marker is beside the child.
    std::fs::write(child_dir.join("marker.txt"), b"beside the child").unwrap();
    let model = parse_pom(child).expect("child parses").model;
    let built = ModelBuilder::new(&source, BuildContext::empty())
        .build(SourcedModel::new(model, "child/pom.xml").with_basedir(&child_dir))
        .expect("build succeeds");
    assert_eq!(built.active_profiles, ["has-marker"]);
}

#[test]
fn derived_build_paths_and_system_properties_resolve_like_mavens() {
    // All four checked against a real Maven 3.9.9 in one POM. These are the
    // values a POM *derives* — the fields themselves were already right, which
    // is why the gap went unnoticed.
    let workspace = tempfile::tempdir().expect("a temp dir");
    let pom = r#"<project>
         <groupId>g</groupId><artifactId>i</artifactId><version>1</version>
         <build><directory>tgt</directory></build>
         <properties>
           <derived.dir>${project.build.directory}/foo</derived.dir>
           <derived.uri>${project.baseUri}</derived.uri>
           <derived.separator>${file.separator}</derived.separator>
         </properties>
       </project>"#;
    let model = parse_pom(pom).expect("parses").model;
    let built = ModelBuilder::new(&MapModelSource::new(), BuildContext::from_environment())
        .build(SourcedModel::new(model, "test").with_basedir(workspace.path()))
        .expect("build succeeds");

    let derived = |key: &str| built.model.properties.get(key).cloned().unwrap_or_default();

    // `alignToBaseDirectory` runs over the resolved value, so an expression
    // reading `<directory>tgt</directory>` gets a real path — jv used to hand
    // back the fragment `tgt/foo`.
    let expected_dir = workspace.path().join("tgt").join("foo");
    assert_eq!(derived("derived.dir"), expected_dir.to_string_lossy());
    // And the field itself is absolute in the effective model, which Maven's
    // own `help:effective-pom` confirms — path translation aligns it in phase 2.
    assert_eq!(
        built
            .model
            .build
            .as_ref()
            .and_then(|b| b.directory.as_deref()),
        Some(workspace.path().join("tgt").to_string_lossy().as_ref())
    );

    // A directory URI ends in a slash.
    assert!(
        derived("derived.uri").ends_with('/'),
        "got {:?}",
        derived("derived.uri")
    );

    // A JVM system property jv has no JVM for, but the process knows anyway.
    // Leaving it literal put a `${...}` where Maven puts a separator.
    assert_eq!(derived("derived.separator"), std::path::MAIN_SEPARATOR_STR);
}
