//! The POM model.
//!
//! Mirrors Maven's `maven.mdo` schema, narrowed to what affects dependency
//! resolution, effective-POM construction, and `dependency:tree` rendering.
//! Site, licence and developer metadata are deliberately absent: they never
//! change which artifacts get resolved.
//!
//! Two things that look like exceptions are not. `<reporting><plugins>` is kept
//! because `mvn site` runs those plugins and a repository without them cannot
//! run it offline. `<configuration>` is not kept, but it is scanned for
//! coordinates on the way past, because plugins resolve artifacts named in
//! there when they run.
//!
//! Almost every field is optional, because a raw POM is a *fragment*: values
//! arrive later from the parent, from properties, or from a default. The
//! distinction between "absent" and "explicitly set" is load-bearing during
//! inheritance and `<dependencyManagement>` injection, so it is preserved rather
//! than collapsed into defaults at parse time.

use std::collections::BTreeMap;

use crate::coordinates::Dependency;

/// The default `<packaging>` when none is declared.
pub const DEFAULT_PACKAGING: &str = "jar";
/// The default `<groupId>` for a `<plugin>` that declares none.
pub const DEFAULT_PLUGIN_GROUP_ID: &str = "org.apache.maven.plugins";
/// The default `<relativePath>` of a `<parent>`.
pub const DEFAULT_PARENT_RELATIVE_PATH: &str = "../pom.xml";

/// Properties, kept sorted so that anything derived from them is deterministic.
///
/// Maven stores these in an order-losing `Properties` table, so no behavior
/// depends on their order; sorting merely makes jv's own output reproducible.
pub type Properties = BTreeMap<String, String>;

/// A parsed POM, before inheritance or interpolation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub model_version: Option<String>,
    pub parent: Option<Parent>,
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    pub packaging: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub inception_year: Option<String>,
    /// `<prerequisites>`. Only `<maven>` matters, and only because POMs
    /// interpolate `${project.prerequisites.maven}` into plugin versions — the
    /// Apache parent chain does exactly that, and without it the expression
    /// survives into a coordinate and produces a nonsense request.
    pub prerequisites: Option<Prerequisites>,
    pub properties: Properties,
    pub dependencies: Vec<Dependency>,
    pub dependency_management: Vec<Dependency>,
    /// `<modules>`, or Maven 4's `<subprojects>` spelling.
    pub modules: Vec<String>,
    pub build: Option<Build>,
    /// `<reporting><plugins>`, which is where a project declares the reports
    /// `mvn site` runs.
    ///
    /// Kept flat rather than behind a `Reporting` struct because only the
    /// plugins matter here: jv resolves them, it does not run them, and
    /// `<reportSets>` decide which goals execute rather than which artifacts
    /// are needed. They inherit and merge exactly as `<build><plugins>` do,
    /// which is why they are the same type.
    pub reporting_plugins: Vec<Plugin>,
    pub profiles: Vec<Profile>,
    pub repositories: Vec<Repository>,
    pub plugin_repositories: Vec<Repository>,
    pub distribution_management: Option<DistributionManagement>,
}

impl Model {
    /// The declared packaging, or `jar`.
    pub fn packaging_or_default(&self) -> &str {
        self.packaging.as_deref().unwrap_or(DEFAULT_PACKAGING)
    }

    /// The group id, falling back to the parent's as inheritance would.
    ///
    /// This is a convenience for reading a raw POM, not a substitute for
    /// building the effective model: it resolves only the one hop a POM can
    /// state directly.
    pub fn declared_or_parent_group_id(&self) -> Option<&str> {
        self.group_id
            .as_deref()
            .or_else(|| self.parent.as_ref().and_then(|p| p.group_id.as_deref()))
    }

    /// The version, falling back to the parent's.
    pub fn declared_or_parent_version(&self) -> Option<&str> {
        self.version
            .as_deref()
            .or_else(|| self.parent.as_ref().and_then(|p| p.version.as_deref()))
    }

    /// `groupId:artifactId:version` from what the POM states directly, for error
    /// messages about POMs too broken to build a model from.
    pub fn coordinates_hint(&self) -> String {
        format!(
            "{}:{}:{}",
            self.declared_or_parent_group_id().unwrap_or("[unknown]"),
            self.artifact_id.as_deref().unwrap_or("[unknown]"),
            self.declared_or_parent_version().unwrap_or("[unknown]")
        )
    }
}

/// A `<parent>` reference.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parent {
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    /// Where to look for the parent on disk before consulting a repository.
    ///
    /// Absent means `../pom.xml`; an explicitly empty value disables the local
    /// lookup entirely, which is how a POM says "always take the parent from the
    /// repository".
    pub relative_path: Option<String>,
}

impl Parent {
    /// The path to try on disk, or `None` when the POM opted out.
    pub fn effective_relative_path(&self) -> Option<&str> {
        match self.relative_path.as_deref() {
            None => Some(DEFAULT_PARENT_RELATIVE_PATH),
            Some("") => None,
            Some(path) => Some(path),
        }
    }
}

/// `<build>`, or the subset of it a `<profile>` may carry.
///
/// Maven splits these into `Build` and `BuildBase`; the difference is only the
/// source-directory fields, which are absent from a profile's build section. jv
/// uses one type and leaves them `None` for profiles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Build {
    pub source_directory: Option<String>,
    pub script_source_directory: Option<String>,
    pub test_source_directory: Option<String>,
    pub output_directory: Option<String>,
    pub test_output_directory: Option<String>,
    pub directory: Option<String>,
    pub final_name: Option<String>,
    pub default_goal: Option<String>,
    pub plugins: Vec<Plugin>,
    pub plugin_management: Vec<Plugin>,
    /// Build extensions. jv cannot *load* one — that needs Maven's container —
    /// but `jv sync` fetches them, because Maven loads them before the build
    /// and `mvn -o` fails outright when one is absent.
    pub extensions: Vec<Extension>,
}

impl Build {
    /// Whether this section carries nothing at all, so merging can skip it.
    pub fn is_empty(&self) -> bool {
        *self == Build::default()
    }
}

/// A `<plugin>`.
///
/// Only what is needed to resolve the plugin and its dependencies is modelled.
/// `<executions>` are kept because plugin inheritance consults them: a parent
/// plugin marked `<inherited>false</inherited>` is still inherited when it
/// declares executions. `<configuration>` is not kept as configuration — jv
/// never writes POMs and never runs a plugin — but it is scanned on the way
/// past for coordinates, which is what `configuration_artifacts` holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plugin {
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    /// Whether the plugin also contributes build extensions.
    pub extensions: Option<bool>,
    /// Whether child projects inherit this plugin declaration.
    pub inherited: Option<bool>,
    pub dependencies: Vec<Dependency>,
    pub executions: Vec<PluginExecution>,
    /// Coordinates named anywhere inside `<configuration>`, at any depth.
    ///
    /// A plugin resolves these itself when it runs, and they appear in no
    /// `<dependencies>` block, so nothing else in a POM reveals them. They are
    /// the single largest reason a repository `jv sync` populated cannot build
    /// offline: `maven-compiler-plugin`'s `<annotationProcessorPaths>`,
    /// `animal-sniffer`'s `<signature>`, `maven-remote-resources`'
    /// `<resourceBundles>` — ten of twenty-six corpus projects failed on this
    /// one shape.
    ///
    /// This is a list of things to *download*, not a model of what the build
    /// uses. That is why it is a flat union rather than merged element by
    /// element the way Maven merges configuration: fetching an artifact the
    /// effective configuration turns out not to want costs a download, while
    /// missing one breaks `mvn -o`.
    ///
    /// Only coordinates that identify themselves are found — a `<groupId>` and
    /// `<artifactId>` pair, or a `g:a:v` string. A plugin that names a default
    /// it resolves from its own code, as `spotless` does with
    /// `<palantirJavaFormat/>`, cannot be seen here by any amount of reading.
    pub configuration_artifacts: Vec<Dependency>,
}

/// A `<execution>` within a plugin.
///
/// Modelled only so plugin inheritance and management can merge executions by
/// id; jv does not run them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginExecution {
    pub id: Option<String>,
    pub phase: Option<String>,
    pub goals: Vec<String>,
    pub inherited: Option<bool>,
}

impl PluginExecution {
    /// The id executions merge on, defaulting to Maven's `default`.
    pub fn id_or_default(&self) -> &str {
        self.id.as_deref().unwrap_or("default")
    }

    /// Whether this execution is inherited, falling back to the plugin's own
    /// setting when it states nothing.
    pub fn is_inherited(&self, plugin: &Plugin) -> bool {
        self.inherited.unwrap_or_else(|| plugin.is_inherited())
    }
}

impl Plugin {
    /// The group id, or `org.apache.maven.plugins`.
    pub fn group_id_or_default(&self) -> &str {
        self.group_id.as_deref().unwrap_or(DEFAULT_PLUGIN_GROUP_ID)
    }

    /// Whether children inherit this declaration, defaulting to true.
    pub fn is_inherited(&self) -> bool {
        self.inherited.unwrap_or(true)
    }

    /// The key plugin declarations merge on: `groupId:artifactId`.
    pub fn key(&self) -> String {
        format!(
            "{}:{}",
            self.group_id_or_default(),
            self.artifact_id.as_deref().unwrap_or("")
        )
    }
}

/// A build `<extension>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extension {
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
}

/// A `<profile>`: a conditional overlay on the model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub id: Option<String>,
    pub activation: Option<Activation>,
    pub properties: Properties,
    pub dependencies: Vec<Dependency>,
    pub dependency_management: Vec<Dependency>,
    pub modules: Vec<String>,
    pub build: Option<Build>,
    /// A profile may declare reports too, and activating it must contribute
    /// them exactly as it contributes build plugins.
    pub reporting_plugins: Vec<Plugin>,
    pub repositories: Vec<Repository>,
    pub plugin_repositories: Vec<Repository>,
    pub distribution_management: Option<DistributionManagement>,
}

impl Profile {
    /// The profile id, or the placeholder Maven uses for an unnamed profile.
    pub fn id_or_default(&self) -> &str {
        self.id.as_deref().unwrap_or("default")
    }
}

/// A profile's `<activation>` conditions.
///
/// Several conditions may be present at once, and Maven requires *all* of them
/// to hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activation {
    pub active_by_default: Option<bool>,
    /// A JDK version or range, optionally negated with a leading `!`.
    pub jdk: Option<String>,
    pub os: Option<ActivationOs>,
    pub property: Option<ActivationProperty>,
    pub file: Option<ActivationFile>,
    /// Maven 4 only.
    pub packaging: Option<String>,
    /// Maven 4 only: an expression in Maven 4's condition language. jv targets
    /// Maven 3.9 behavior and does not evaluate these.
    pub condition: Option<String>,
}

impl Activation {
    /// Whether any condition beyond `activeByDefault` is present.
    pub fn has_conditions(&self) -> bool {
        self.jdk.is_some()
            || self.os.is_some()
            || self.property.is_some()
            || self.file.is_some()
            || self.packaging.is_some()
            || self.condition.is_some()
    }
}

/// Operating-system conditions. Each present field must match; a value may be
/// negated with a leading `!`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivationOs {
    pub name: Option<String>,
    pub family: Option<String>,
    pub arch: Option<String>,
    pub version: Option<String>,
}

/// A property condition.
///
/// With no `<value>`, the condition tests presence; a name prefixed with `!`
/// negates it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivationProperty {
    pub name: Option<String>,
    pub value: Option<String>,
}

/// A file-existence condition. Both paths are interpolated before testing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivationFile {
    pub missing: Option<String>,
    pub exists: Option<String>,
}

/// A `<repository>` or `<pluginRepository>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repository {
    pub id: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    /// `default` (the Maven 2 layout) or `legacy`. jv supports only `default`.
    pub layout: Option<String>,
    pub releases: Option<RepositoryPolicy>,
    pub snapshots: Option<RepositoryPolicy>,
}

/// Per-repository handling of releases or snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryPolicy {
    /// Defaults to true.
    pub enabled: Option<bool>,
    /// `always`, `daily` (the default), `never`, or `interval:<minutes>`.
    pub update_policy: Option<String>,
    /// `fail`, `warn` (the default) or `ignore`.
    pub checksum_policy: Option<String>,
}

/// `<distributionManagement>`, narrowed to the parts that change resolution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DistributionManagement {
    /// Where this artifact moved to. Resolution follows relocations, so this is
    /// not optional to support.
    pub relocation: Option<Relocation>,
    /// A free-text status such as `deployed`; never affects resolution.
    pub status: Option<String>,
}

/// `<prerequisites>`: the minimum Maven a build declares it needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prerequisites {
    pub maven: Option<String>,
}

/// A `<relocation>`: any absent field keeps the original coordinate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Relocation {
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaging_defaults_to_jar() {
        assert_eq!(Model::default().packaging_or_default(), "jar");
    }

    #[test]
    fn parent_relative_path_defaults_and_opt_out() {
        let mut parent = Parent::default();
        assert_eq!(parent.effective_relative_path(), Some("../pom.xml"));
        parent.relative_path = Some("../../pom.xml".to_owned());
        assert_eq!(parent.effective_relative_path(), Some("../../pom.xml"));
        // An explicitly empty relativePath means "do not look on disk".
        parent.relative_path = Some(String::new());
        assert_eq!(parent.effective_relative_path(), None);
    }

    #[test]
    fn plugin_group_id_defaults() {
        let plugin = Plugin {
            artifact_id: Some("maven-compiler-plugin".to_owned()),
            ..Default::default()
        };
        assert_eq!(plugin.group_id_or_default(), "org.apache.maven.plugins");
        assert_eq!(
            plugin.key(),
            "org.apache.maven.plugins:maven-compiler-plugin"
        );
        assert!(plugin.is_inherited());
    }

    #[test]
    fn coordinates_fall_back_to_parent() {
        let model = Model {
            artifact_id: Some("child".to_owned()),
            parent: Some(Parent {
                group_id: Some("com.example".to_owned()),
                artifact_id: Some("parent".to_owned()),
                version: Some("1.0".to_owned()),
                relative_path: None,
            }),
            ..Default::default()
        };
        assert_eq!(model.declared_or_parent_group_id(), Some("com.example"));
        assert_eq!(model.declared_or_parent_version(), Some("1.0"));
        assert_eq!(model.coordinates_hint(), "com.example:child:1.0");
    }

    #[test]
    fn activation_conditions_are_detected() {
        let mut activation = Activation {
            active_by_default: Some(true),
            ..Default::default()
        };
        assert!(!activation.has_conditions());
        activation.jdk = Some("11".to_owned());
        assert!(activation.has_conditions());
    }
}
