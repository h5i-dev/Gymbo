//! `${...}` expansion.
//!
//! Ported from Maven 3.9's `AbstractStringBasedModelInterpolator`, **not** from
//! Maven 4's `DefaultModelInterpolator`. The two order their value sources
//! differently and jv targets 3.9:
//!
//! | Priority | Maven 3.9 | Maven 4 |
//! |---|---|---|
//! | 1 | `basedir` / `baseUri` / build timestamp | same |
//! | 2 | **`project.*` / `pom.*` model reflection** | user properties (`-D`) |
//! | 3 | user properties (`-D`) | model `<properties>` |
//! | 4 | model `<properties>` | **`project.*` model reflection** |
//! | 5 | system properties | same |
//! | 6 | `env.*` fallback | same |
//! | 7 | un-prefixed model reflection | same |
//!
//! The consequence is observable: under Maven 3.9 neither `-Dproject.version=x`
//! nor a POM property named `project.version` can shadow the real project
//! version, while under Maven 4 both can. Getting this backwards would change
//! resolved versions on real projects, so it is worth stating twice.
//!
//! Maven 4's `${var:-default}` / `${var:+alt}` operators and `\${` escaping do
//! not exist in 3.9 and are deliberately not implemented.

use std::collections::HashMap;
use std::path::Path;

use jv_model::{Dependency, Model};

use crate::context::BuildContext;
use crate::problem::Problem;

/// Prefixes that address the model itself, in Maven 3.9's lookup order.
///
/// `pom.` is deprecated in favour of `project.` but still resolves in 3.9.
const PROJECT_PREFIXES: &[&str] = &["pom.", "project."];

/// Expands `${...}` against a model and its environment.
/// How deep a chain of `${a}` -> `${b}` -> ... may go.
///
/// Real POMs are two or three. The bound is here because expansion recurses, and
/// a POM built to be deep would otherwise overflow the stack — which with
/// `panic = "abort"` is a process abort rather than an error.
const MAX_EXPANSION_DEPTH: usize = 128;

/// The expressions currently being expanded, for cycle detection.
///
/// A vector for the order and a set for the membership test. The linear scan it
/// replaces was O(depth) at every level, which is another factor of n on a POM
/// whose properties form a long chain.
#[derive(Debug, Default)]
struct Stack {
    order: Vec<String>,
    held: std::collections::HashSet<String>,
}

impl Stack {
    fn contains(&self, expression: &str) -> bool {
        self.held.contains(expression)
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn push(&mut self, expression: &str) {
        self.order.push(expression.to_owned());
        self.held.insert(expression.to_owned());
    }

    fn pop(&mut self, expression: &str) {
        self.order.pop();
        self.held.remove(expression);
    }
}

pub struct Interpolator<'a> {
    model: &'a Model,
    basedir: Option<&'a Path>,
    context: &'a BuildContext,
    /// Used to attribute problems to a POM.
    source: String,
    /// Expressions already expanded, and what they came to.
    ///
    /// An expression's value does not depend on where it was reached from, so
    /// once is enough. Without this a chain of properties each referring to the
    /// next is re-expanded from every entry point, which made a 100 KB POM take
    /// 25 seconds where Maven takes two — `StringSearchInterpolator` caches for
    /// exactly this reason. Only completed expansions are stored; a cycle
    /// resolves to nothing and must be reported every time it is reached.
    resolved: std::cell::RefCell<HashMap<String, String>>,
}

impl std::fmt::Debug for Interpolator<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interpolator")
            .field("source", &self.source)
            .field("basedir", &self.basedir)
            .finish_non_exhaustive()
    }
}

impl<'a> Interpolator<'a> {
    pub fn new(
        model: &'a Model,
        basedir: Option<&'a Path>,
        context: &'a BuildContext,
        source: impl Into<String>,
    ) -> Self {
        Self {
            model,
            basedir,
            context,
            source: source.into(),
            resolved: std::cell::RefCell::default(),
        }
    }

    /// Expands every placeholder in `text`.
    ///
    /// An expression that resolves to nothing is left exactly as written, which
    /// is why effective POMs sometimes still show a literal `${...}`. A recursive
    /// reference is reported and also left as written.
    pub fn interpolate(&self, text: &str, problems: &mut Vec<Problem>) -> String {
        // Most strings in a POM contain no placeholder at all.
        if !text.contains('$') {
            return text.to_owned();
        }
        let mut stack = Stack::default();
        self.expand(text, &mut stack, problems)
    }

    /// Expands `text`, ignoring the result when nothing changed.
    fn interpolate_in_place(&self, slot: &mut Option<String>, problems: &mut Vec<Problem>) {
        if let Some(value) = slot {
            if value.contains('$') {
                *value = self.interpolate(value, problems);
            }
        }
    }

    fn expand(&self, text: &str, stack: &mut Stack, problems: &mut Vec<Problem>) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            let Some(start) = rest.find("${") else {
                out.push_str(rest);
                return out;
            };
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                // Unterminated placeholder: Maven leaves the text alone.
                out.push_str(&rest[start..]);
                return out;
            };
            let expression = &after[..end];
            match self.resolve(expression, stack, problems) {
                Some(value) => out.push_str(&value),
                None => {
                    out.push_str("${");
                    out.push_str(expression);
                    out.push('}');
                }
            }
            rest = &after[end + 1..];
        }
    }

    fn resolve(
        &self,
        expression: &str,
        stack: &mut Stack,
        problems: &mut Vec<Problem>,
    ) -> Option<String> {
        if let Some(cached) = self.resolved.borrow().get(expression) {
            return Some(cached.clone());
        }
        if stack.contains(expression) {
            problems.push(Problem::error(
                &self.source,
                format!("recursive variable reference: {expression}"),
            ));
            return None;
        }
        // A property chain is expanded by recursion, so its depth is the chain's
        // length. Bounded so a POM built to be deep cannot overflow the stack —
        // which with `panic = "abort"` would be a process abort, not an error.
        if stack.len() >= MAX_EXPANSION_DEPTH {
            problems.push(Problem::error(
                &self.source,
                format!(
                    "expression {expression} is nested more than {MAX_EXPANSION_DEPTH} deep and was left unexpanded"
                ),
            ));
            return None;
        }

        let raw = self.lookup(expression)?;
        // A resolved value may itself contain placeholders.
        if !raw.contains('$') {
            self.remember(expression, &raw);
            return Some(raw);
        }
        stack.push(expression);
        let expanded = self.expand(&raw, stack, problems);
        stack.pop(expression);
        self.remember(expression, &expanded);
        Some(expanded)
    }

    fn remember(&self, expression: &str, value: &str) {
        self.resolved
            .borrow_mut()
            .insert(expression.to_owned(), value.to_owned());
    }

    /// One expression, in Maven 3.9's source order.
    fn lookup(&self, expression: &str) -> Option<String> {
        // 1. The bare basedir, and the build timestamp.
        if expression == "basedir" {
            return self.basedir_string();
        }
        // `maven.build.timestamp` has no clock behind it here on purpose: jv
        // resolves dependencies, and a build stamped with the current time would
        // make that non-reproducible. A caller that needs one can supply it as a
        // user property, which resolves at step 3.

        // 2. Prefixed model reflection — above the property sources in 3.9.
        for prefix in PROJECT_PREFIXES {
            if let Some(path) = expression.strip_prefix(prefix) {
                if let Some(value) = self.model_property(path, true) {
                    return Some(value);
                }
            }
        }
        // 3. Command-line properties.
        if let Some(value) = self.context.user_properties.get(expression) {
            return Some(value.clone());
        }
        // 4. The model's own <properties>.
        if let Some(value) = self.model.properties.get(expression) {
            return Some(value.clone());
        }
        // 5. System properties, which already contain `env.`-prefixed keys.
        if let Some(value) = self.context.system_properties.get(expression) {
            return Some(value.clone());
        }
        // 6. `${PATH}` as a fallback spelling of `${env.PATH}`.
        if let Some(value) = self
            .context
            .system_properties
            .get(&format!("env.{expression}"))
        {
            return Some(value.clone());
        }
        // 7. Un-prefixed model reflection, for legacy `${version}` and friends.
        self.model_property(expression, false)
    }

    fn basedir_string(&self) -> Option<String> {
        self.basedir.map(|dir| dir.to_string_lossy().into_owned())
    }

    /// A build path, resolved against the project directory.
    ///
    /// Left alone when it is already absolute, or when there is no directory to
    /// resolve against — a POM read out of a repository has none.
    fn aligned(&self, path: Option<&str>) -> Option<String> {
        let path = path?;
        let (Some(basedir), false) = (self.basedir, Path::new(path).is_absolute()) else {
            return Some(path.to_owned());
        };
        Some(basedir.join(path).to_string_lossy().into_owned())
    }

    /// Evaluates a path against the model.
    ///
    /// `basedir` and `baseUri` resolve only when reached through a `project.` or
    /// `pom.` prefix, matching upstream's split between its prefixed and
    /// un-prefixed value sources — except for bare `basedir`, handled above.
    fn model_property(&self, path: &str, prefixed: bool) -> Option<String> {
        let model = self.model;
        if prefixed {
            match path {
                "basedir" => return self.basedir_string(),
                "baseUri" => {
                    return self.basedir.map(|dir| {
                        // Maven renders this as a directory URI, and a directory
                        // URI ends in `/`. Without it, `${project.baseUri}src`
                        // yields a sibling of the project rather than a path
                        // inside it.
                        format!("file://{}/", dir.to_string_lossy().trim_end_matches('/'))
                    });
                }
                _ => {}
            }
        }
        match path {
            "modelVersion" => model.model_version.clone(),
            "groupId" => model.declared_or_parent_group_id().map(str::to_owned),
            "artifactId" => model.artifact_id.clone(),
            "version" => model.declared_or_parent_version().map(str::to_owned),
            "packaging" => Some(model.packaging_or_default().to_owned()),
            "name" => model.name.clone(),
            "description" => model.description.clone(),
            "url" => model.url.clone(),
            "inceptionYear" => model.inception_year.clone(),
            // The Apache parent chain interpolates this into plugin versions.
            // Without it the expression survives into a coordinate, and the
            // first thing that notices is a request for a URL containing the
            // mangled remains of `${...}`.
            "prerequisites.maven" => model.prerequisites.as_ref()?.maven.clone(),
            "parent.groupId" => model.parent.as_ref()?.group_id.clone(),
            "parent.artifactId" => model.parent.as_ref()?.artifact_id.clone(),
            "parent.version" => model.parent.as_ref()?.version.clone(),
            // Build paths come back absolute. Maven's `alignToBaseDirectory`
            // runs over the *resolved* value, so `${project.build.directory}/x`
            // is a real path and not a fragment relative to nothing — the POM's
            // own `<directory>` stays as written, but every expression reading it
            // gets the aligned form.
            "build.directory" => self.aligned(model.build.as_ref()?.directory.as_deref()),
            "build.outputDirectory" => {
                self.aligned(model.build.as_ref()?.output_directory.as_deref())
            }
            "build.testOutputDirectory" => {
                self.aligned(model.build.as_ref()?.test_output_directory.as_deref())
            }
            "build.sourceDirectory" => {
                self.aligned(model.build.as_ref()?.source_directory.as_deref())
            }
            "build.testSourceDirectory" => {
                self.aligned(model.build.as_ref()?.test_source_directory.as_deref())
            }
            "build.scriptSourceDirectory" => {
                self.aligned(model.build.as_ref()?.script_source_directory.as_deref())
            }
            "build.finalName" => model.build.as_ref()?.final_name.clone(),
            "build.defaultGoal" => model.build.as_ref()?.default_goal.clone(),
            "distributionManagement.status" => {
                model.distribution_management.as_ref()?.status.clone()
            }
            // `${project.properties.foo}` addresses the property map.
            other => other
                .strip_prefix("properties.")
                .and_then(|key| model.properties.get(key).cloned()),
        }
    }
}

/// Interpolates every string in a model in place.
///
/// Property *keys* are left alone; only values are expanded, matching upstream.
pub fn interpolate_model(
    model: &mut Model,
    basedir: Option<&Path>,
    context: &BuildContext,
    source: &str,
    problems: &mut Vec<Problem>,
) {
    // The interpolator reads the model while the model is being rewritten, so it
    // works from a snapshot. Upstream has the same property: a value produced by
    // interpolation never becomes a source for another expression in the same
    // pass.
    let snapshot = model.clone();
    let interpolator = Interpolator::new(&snapshot, basedir, context, source);

    for slot in [
        &mut model.group_id,
        &mut model.artifact_id,
        &mut model.version,
        &mut model.packaging,
        &mut model.name,
        &mut model.description,
        &mut model.url,
        &mut model.inception_year,
    ] {
        interpolator.interpolate_in_place(slot, problems);
    }

    if let Some(parent) = &mut model.parent {
        for slot in [
            &mut parent.group_id,
            &mut parent.artifact_id,
            &mut parent.version,
            &mut parent.relative_path,
        ] {
            interpolator.interpolate_in_place(slot, problems);
        }
    }

    for value in model.properties.values_mut() {
        if value.contains('$') {
            *value = interpolator.interpolate(value, problems);
        }
    }

    for module in &mut model.modules {
        if module.contains('$') {
            *module = interpolator.interpolate(module, problems);
        }
    }

    for dependency in model
        .dependencies
        .iter_mut()
        .chain(&mut model.dependency_management)
    {
        interpolate_dependency(dependency, &interpolator, problems);
    }

    // Report plugins carry versions and configuration coordinates like any
    // others, and a `${...}` left in one becomes a request for a path that
    // cannot exist.
    for plugin in &mut model.reporting_plugins {
        for slot in [
            &mut plugin.group_id,
            &mut plugin.artifact_id,
            &mut plugin.version,
        ] {
            interpolator.interpolate_in_place(slot, problems);
        }
        for dependency in plugin
            .dependencies
            .iter_mut()
            .chain(&mut plugin.configuration_artifacts)
        {
            interpolate_dependency(dependency, &interpolator, problems);
        }
    }

    if let Some(build) = &mut model.build {
        for slot in [
            &mut build.source_directory,
            &mut build.script_source_directory,
            &mut build.test_source_directory,
            &mut build.output_directory,
            &mut build.test_output_directory,
            &mut build.directory,
            &mut build.final_name,
            &mut build.default_goal,
        ] {
            interpolator.interpolate_in_place(slot, problems);
        }
        for plugin in build.plugins.iter_mut().chain(&mut build.plugin_management) {
            for slot in [
                &mut plugin.group_id,
                &mut plugin.artifact_id,
                &mut plugin.version,
            ] {
                interpolator.interpolate_in_place(slot, problems);
            }
            for dependency in plugin
                .dependencies
                .iter_mut()
                .chain(&mut plugin.configuration_artifacts)
            {
                interpolate_dependency(dependency, &interpolator, problems);
            }
        }
        for extension in &mut build.extensions {
            for slot in [
                &mut extension.group_id,
                &mut extension.artifact_id,
                &mut extension.version,
            ] {
                interpolator.interpolate_in_place(slot, problems);
            }
        }
    }

    for repository in model
        .repositories
        .iter_mut()
        .chain(&mut model.plugin_repositories)
    {
        for slot in [
            &mut repository.id,
            &mut repository.name,
            &mut repository.url,
            &mut repository.layout,
        ] {
            interpolator.interpolate_in_place(slot, problems);
        }
    }

    if let Some(management) = &mut model.distribution_management {
        interpolator.interpolate_in_place(&mut management.status, problems);
        if let Some(relocation) = &mut management.relocation {
            for slot in [
                &mut relocation.group_id,
                &mut relocation.artifact_id,
                &mut relocation.version,
                &mut relocation.message,
            ] {
                interpolator.interpolate_in_place(slot, problems);
            }
        }
    }
}

fn interpolate_dependency(
    dependency: &mut Dependency,
    interpolator: &Interpolator<'_>,
    problems: &mut Vec<Problem>,
) {
    if dependency.group_id.contains('$') {
        dependency.group_id = interpolator.interpolate(&dependency.group_id, problems);
    }
    if dependency.artifact_id.contains('$') {
        dependency.artifact_id = interpolator.interpolate(&dependency.artifact_id, problems);
    }
    for slot in [
        &mut dependency.version,
        &mut dependency.type_,
        &mut dependency.classifier,
        &mut dependency.system_path,
    ] {
        interpolator.interpolate_in_place(slot, problems);
    }
    for exclusion in &mut dependency.exclusions {
        if exclusion.group_id.contains('$') {
            exclusion.group_id = interpolator.interpolate(&exclusion.group_id, problems);
        }
        if exclusion.artifact_id.contains('$') {
            exclusion.artifact_id = interpolator.interpolate(&exclusion.artifact_id, problems);
        }
    }
}

/// Resolves CI-friendly versions before the parent chain is walked.
///
/// Maven 3.5 introduced `<version>${revision}</version>` (and `${sha1}` /
/// `${changelist}`), which has to work *before* inheritance because the parent
/// reference itself may use it. Only properties and the command line are
/// available at that point — there is no model to reflect over yet.
pub fn replace_ci_friendly_version(model: &mut Model, context: &BuildContext) {
    let lookup = |expression: &str| -> Option<String> {
        context
            .user_properties
            .get(expression)
            .or_else(|| model.properties.get(expression))
            .or_else(|| context.system_properties.get(expression))
            .cloned()
    };

    let expand = |text: &str| -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            let Some(start) = rest.find("${") else {
                out.push_str(rest);
                return out;
            };
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                out.push_str(&rest[start..]);
                return out;
            };
            let expression = &after[..end];
            match lookup(expression) {
                Some(value) => out.push_str(&value),
                None => {
                    out.push_str("${");
                    out.push_str(expression);
                    out.push('}');
                }
            }
            rest = &after[end + 1..];
        }
    };

    let new_version = model
        .version
        .as_deref()
        .filter(|v| v.contains('$'))
        .map(&expand);
    let new_parent_version = model
        .parent
        .as_ref()
        .and_then(|parent| parent.version.as_deref())
        .filter(|v| v.contains('$'))
        .map(expand);

    if let Some(version) = new_version {
        model.version = Some(version);
    }
    if let Some(version) = new_parent_version {
        if let Some(parent) = &mut model.parent {
            parent.version = Some(version);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::parse_pom;

    fn model_of(xml: &str) -> Model {
        parse_pom(xml).expect("parse").model
    }

    fn interpolate(model: &Model, context: &BuildContext, text: &str) -> (String, Vec<Problem>) {
        let mut problems = Vec::new();
        let interpolator = Interpolator::new(model, None, context, "test");
        let value = interpolator.interpolate(text, &mut problems);
        (value, problems)
    }

    #[test]
    fn plain_text_is_untouched() {
        let model = Model::default();
        let (value, problems) = interpolate(&model, &BuildContext::empty(), "no placeholders");
        assert_eq!(value, "no placeholders");
        assert!(problems.is_empty());
    }

    #[test]
    fn model_reflection_and_both_prefixes() {
        let model = model_of(
            r#"<project><groupId>g</groupId><artifactId>a</artifactId><version>1.2</version></project>"#,
        );
        let context = BuildContext::empty();
        assert_eq!(interpolate(&model, &context, "${project.version}").0, "1.2");
        // `pom.` is deprecated but still resolves under Maven 3.9.
        assert_eq!(interpolate(&model, &context, "${pom.version}").0, "1.2");
        assert_eq!(interpolate(&model, &context, "${project.groupId}").0, "g");
        // Un-prefixed legacy form.
        assert_eq!(interpolate(&model, &context, "${version}").0, "1.2");
    }

    #[test]
    fn properties_resolve_and_nest() {
        let model = model_of(
            r#"<project><properties>
                 <base>1</base>
                 <derived>${base}.5</derived>
               </properties></project>"#,
        );
        let context = BuildContext::empty();
        assert_eq!(interpolate(&model, &context, "${derived}").0, "1.5");
        assert_eq!(
            interpolate(&model, &context, "v${derived}-final").0,
            "v1.5-final"
        );
    }

    #[test]
    fn maven_3_puts_model_reflection_above_properties() {
        // This is the divergence from Maven 4: neither -D nor a POM property can
        // shadow the real project version under 3.9.
        let model = model_of(
            r#"<project>
                 <version>1.0</version>
                 <properties><project.version>shadowed</project.version></properties>
               </project>"#,
        );
        let context = BuildContext::empty().with_user_property("project.version", "from-cli");
        assert_eq!(interpolate(&model, &context, "${project.version}").0, "1.0");
    }

    #[test]
    fn user_properties_beat_model_properties() {
        let model = model_of(r#"<project><properties><flag>pom</flag></properties></project>"#);
        let context = BuildContext::empty().with_user_property("flag", "cli");
        assert_eq!(interpolate(&model, &context, "${flag}").0, "cli");
    }

    #[test]
    fn model_properties_beat_system_properties() {
        let model = model_of(r#"<project><properties><flag>pom</flag></properties></project>"#);
        let context = BuildContext::empty().with_system_property("flag", "system");
        assert_eq!(interpolate(&model, &context, "${flag}").0, "pom");
    }

    #[test]
    fn environment_variables_resolve_with_and_without_prefix() {
        let model = Model::default();
        let context = BuildContext::empty().with_system_property("env.CI", "true");
        assert_eq!(interpolate(&model, &context, "${env.CI}").0, "true");
        // The bare spelling falls back to the env-prefixed key.
        assert_eq!(interpolate(&model, &context, "${CI}").0, "true");
    }

    #[test]
    fn unresolvable_expressions_survive_verbatim() {
        let model = Model::default();
        let (value, problems) = interpolate(&model, &BuildContext::empty(), "a-${nope}-b");
        assert_eq!(value, "a-${nope}-b");
        // Upstream reports nothing for an unresolved expression.
        assert!(problems.is_empty());
    }

    #[test]
    fn unterminated_placeholder_is_left_alone() {
        let model = Model::default();
        assert_eq!(
            interpolate(&model, &BuildContext::empty(), "a-${nope").0,
            "a-${nope"
        );
    }

    #[test]
    fn recursion_is_reported_and_left_as_written() {
        let model = model_of(
            r#"<project><properties>
                 <a>${b}</a>
                 <b>${a}</b>
               </properties></project>"#,
        );
        let (value, problems) = interpolate(&model, &BuildContext::empty(), "${a}");
        assert!(problems.iter().any(|p| p.message.contains("recursive")));
        // Maven 3 leaves the text in place rather than nulling the field.
        assert!(value.contains("${"), "got {value}");
    }

    #[test]
    fn basedir_resolves_prefixed_and_bare() {
        let model = Model::default();
        let context = BuildContext::empty();
        let basedir = Path::new("/work/project");
        let interpolator = Interpolator::new(&model, Some(basedir), &context, "test");
        let mut problems = Vec::new();
        assert_eq!(
            interpolator.interpolate("${basedir}", &mut problems),
            "/work/project"
        );
        assert_eq!(
            interpolator.interpolate("${project.basedir}", &mut problems),
            "/work/project"
        );
        assert_eq!(
            interpolator.interpolate("${project.baseUri}", &mut problems),
            // Trailing slash: it is a *directory* URI, and Maven writes one.
            // `${project.baseUri}src` must land inside the project.
            "file:///work/project/"
        );
        // baseUri has no un-prefixed spelling.
        assert_eq!(
            interpolator.interpolate("${baseUri}", &mut problems),
            "${baseUri}"
        );
    }

    #[test]
    fn build_directories_resolve() {
        let model = model_of(
            r#"<project><build>
                 <directory>/t</directory>
                 <finalName>app</finalName>
               </build></project>"#,
        );
        let context = BuildContext::empty();
        assert_eq!(
            interpolate(&model, &context, "${project.build.directory}").0,
            "/t"
        );
        assert_eq!(
            interpolate(&model, &context, "${project.build.finalName}").0,
            "app"
        );
    }

    #[test]
    fn whole_model_is_interpolated() {
        let mut model = model_of(
            r#"<project>
                 <groupId>com.example</groupId>
                 <artifactId>app</artifactId>
                 <version>1.0</version>
                 <properties><dep.version>2.5</dep.version></properties>
                 <dependencies>
                   <dependency>
                     <groupId>${project.groupId}</groupId>
                     <artifactId>lib</artifactId>
                     <version>${dep.version}</version>
                     <exclusions>
                       <exclusion><groupId>${project.groupId}</groupId><artifactId>x</artifactId></exclusion>
                     </exclusions>
                   </dependency>
                 </dependencies>
                 <build><plugins><plugin>
                   <artifactId>p</artifactId><version>${dep.version}</version>
                 </plugin></plugins></build>
               </project>"#,
        );
        let mut problems = Vec::new();
        interpolate_model(
            &mut model,
            None,
            &BuildContext::empty(),
            "test",
            &mut problems,
        );
        let dependency = &model.dependencies[0];
        assert_eq!(dependency.group_id, "com.example");
        assert_eq!(dependency.version.as_deref(), Some("2.5"));
        assert_eq!(dependency.exclusions[0].group_id, "com.example");
        assert_eq!(
            model.build.as_ref().unwrap().plugins[0].version.as_deref(),
            Some("2.5")
        );
        assert!(problems.is_empty());
    }

    #[test]
    fn property_keys_are_not_interpolated() {
        let mut model = model_of(
            r#"<project><properties>
                 <a>x</a>
                 <b>${a}</b>
               </properties></project>"#,
        );
        let mut problems = Vec::new();
        interpolate_model(
            &mut model,
            None,
            &BuildContext::empty(),
            "test",
            &mut problems,
        );
        assert_eq!(model.properties.get("b").unwrap(), "x");
        assert!(model.properties.contains_key("a"));
    }

    #[test]
    fn ci_friendly_versions_resolve_before_inheritance() {
        let mut model = model_of(
            r#"<project>
                 <parent>
                   <groupId>g</groupId><artifactId>p</artifactId><version>${revision}</version>
                 </parent>
                 <artifactId>child</artifactId>
                 <version>${revision}</version>
                 <properties><revision>3.1.4</revision></properties>
               </project>"#,
        );
        replace_ci_friendly_version(&mut model, &BuildContext::empty());
        assert_eq!(model.version.as_deref(), Some("3.1.4"));
        assert_eq!(
            model.parent.as_ref().unwrap().version.as_deref(),
            Some("3.1.4")
        );
    }

    #[test]
    fn ci_friendly_version_prefers_the_command_line() {
        let mut model = model_of(
            r#"<project>
                 <version>${revision}</version>
                 <properties><revision>1.0-SNAPSHOT</revision></properties>
               </project>"#,
        );
        let context = BuildContext::empty().with_user_property("revision", "9.9.9");
        replace_ci_friendly_version(&mut model, &context);
        assert_eq!(model.version.as_deref(), Some("9.9.9"));
    }

    #[test]
    fn ci_friendly_version_left_alone_when_undefined() {
        let mut model = model_of(r#"<project><version>${revision}</version></project>"#);
        replace_ci_friendly_version(&mut model, &BuildContext::empty());
        assert_eq!(model.version.as_deref(), Some("${revision}"));
    }
}
