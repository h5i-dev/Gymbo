//! Streaming POM parser.
//!
//! POMs are numerous and small, and most of a real POM's bytes are plugin
//! `<configuration>` that jv has no use for. A streaming reader that skips
//! whole subtrees is therefore both faster and simpler than building a document
//! tree and walking it — the same choice coursier made in its `PomParser`.
//!
//! Parsing is deliberately lenient. Anything unrecognized is skipped rather than
//! rejected, because the Maven Central corpus contains two decades of POMs
//! written against older schemas, and refusing to read one that Maven reads
//! happily would be a compatibility bug. Problems Maven reports as warnings are
//! collected in [`ParsedPom::warnings`] instead of failing the parse.

use quick_xml::Reader;
use quick_xml::events::Event;
use quick_xml::name::QName;

use crate::coordinates::{Dependency, Exclusion};
use crate::model::{
    Activation, ActivationFile, ActivationOs, ActivationProperty, Build, DistributionManagement,
    Extension, Model, Parent, Plugin, PluginExecution, Prerequisites, Profile, Relocation,
    Repository, RepositoryPolicy,
};
use crate::scope::Scope;

/// A POM that failed to parse.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("malformed XML: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("malformed XML text: {0}")]
    Escape(#[from] quick_xml::escape::EscapeError),
    #[error("expected a <{expected}> root element, found <{found}>")]
    UnexpectedRoot {
        expected: &'static str,
        found: String,
    },
    #[error("document is empty")]
    Empty,
    #[error("unexpected end of document inside <{element}>")]
    UnexpectedEof { element: String },
}

/// A parsed POM together with anything questionable found along the way.
#[derive(Debug)]
pub struct ParsedPom {
    pub model: Model,
    /// Problems Maven would report as warnings, such as an unrecognized
    /// `<scope>`. Parsing continued past each of them.
    pub warnings: Vec<String>,
}

/// Parses a POM.
///
/// # Examples
///
/// ```
/// use jv_model::parse_pom;
///
/// let pom = parse_pom(r#"
///     <project>
///       <groupId>com.example</groupId>
///       <artifactId>demo</artifactId>
///       <version>1.0</version>
///       <dependencies>
///         <dependency>
///           <groupId>org.slf4j</groupId>
///           <artifactId>slf4j-api</artifactId>
///           <version>2.0.9</version>
///         </dependency>
///       </dependencies>
///     </project>
/// "#).unwrap();
///
/// assert_eq!(pom.model.artifact_id.as_deref(), Some("demo"));
/// assert_eq!(pom.model.dependencies.len(), 1);
/// assert_eq!(pom.model.dependencies[0].artifact_id, "slf4j-api");
/// ```
pub fn parse_pom(xml: &str) -> Result<ParsedPom, ParseError> {
    let mut parser = XmlParser::new(xml);
    parser.enter_root("project")?;
    let model = parser.parse_project()?;
    Ok(ParsedPom {
        model,
        warnings: parser.warnings,
    })
}

/// Maven's boolean parsing: only `true`, case-insensitively, is true.
///
/// This matches `Boolean.parseBoolean`, which means `<optional>yes</optional>`
/// reads as false rather than as an error.
fn parse_bool(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("true")
}

pub(crate) struct XmlParser<'i> {
    reader: Reader<&'i [u8]>,
    warnings: Vec<String>,
}

impl<'i> XmlParser<'i> {
    pub(crate) fn new(xml: &'i str) -> Self {
        let mut reader = Reader::from_str(xml);
        let config = reader.config_mut();
        // Self-closing elements become Start+End, so every handler can assume a
        // body is present and read to its end.
        config.expand_empty_elements = true;
        // Mismatched tags are a real error, not something to recover from.
        config.check_end_names = true;
        Self {
            reader,
            warnings: Vec::new(),
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// Stores a repeated wrapper element's contents, saying so if it replaces
    /// something.
    ///
    /// Maven's generated reader rejects a duplicated tag outright, so a POM that
    /// declares `<dependencies>` twice is one Maven will not read at all. jv is
    /// lenient and keeps the last, but silently resolving a dependency set that
    /// Maven refuses is the worst of both — the warning is what makes the
    /// difference visible.
    fn replace_repeated<T>(&mut self, name: &str, slot: &mut Vec<T>, parsed: Vec<T>) {
        if !slot.is_empty() {
            self.warn(format!(
                "<{name}> appears more than once; Maven rejects that, and jv is \
                 using only the last"
            ));
        }
        *slot = parsed;
    }

    fn read_event(&mut self) -> Result<Event<'i>, ParseError> {
        Ok(self.reader.read_event()?)
    }

    /// Advances to the document's root element, requiring it to be `expected`.
    ///
    /// On return the root's children are next, so the caller can hand straight to
    /// [`XmlParser::children`].
    pub(crate) fn enter_root(&mut self, expected: &'static str) -> Result<(), ParseError> {
        loop {
            match self.read_event()? {
                Event::Start(element) => {
                    let name = element.local_name();
                    if name.as_ref() != expected.as_bytes() {
                        return Err(ParseError::UnexpectedRoot {
                            expected,
                            found: String::from_utf8_lossy(name.as_ref()).into_owned(),
                        });
                    }
                    return Ok(());
                }
                Event::Eof => return Err(ParseError::Empty),
                // Skip the declaration, comments, doctype and stray whitespace.
                _ => {}
            }
        }
    }

    /// Visits the direct children of the element currently being read.
    ///
    /// `visit` returns whether it consumed the child through its end tag; when it
    /// returns false the whole subtree is skipped, which is how unrecognized
    /// elements — most importantly plugin `<configuration>` — cost nothing.
    pub(crate) fn children<F>(&mut self, context: &str, mut visit: F) -> Result<(), ParseError>
    where
        F: FnMut(&mut Self, &[u8]) -> Result<bool, ParseError>,
    {
        loop {
            match self.read_event()? {
                Event::Start(element) => {
                    let qname = element.name();
                    let local = element.local_name();
                    if !visit(self, local.as_ref())? {
                        self.skip_to_end(qname)?;
                    }
                }
                Event::End(_) => return Ok(()),
                Event::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        element: context.to_owned(),
                    });
                }
                _ => {}
            }
        }
    }

    fn skip_to_end(&mut self, name: QName<'_>) -> Result<(), ParseError> {
        self.reader.read_to_end(name)?;
        Ok(())
    }

    /// Reads the text content of the element currently being read, trimmed.
    ///
    /// Nested markup is ignored rather than rejected: a POM that puts elements
    /// inside a text field is malformed, but Maven reads it without complaint.
    pub(crate) fn text(&mut self) -> Result<String, ParseError> {
        let mut out = String::new();
        let mut depth = 0usize;
        loop {
            match self.read_event()? {
                Event::Text(text) if depth == 0 => out.push_str(&text.unescape()?),
                Event::CData(data) if depth == 0 => {
                    out.push_str(&String::from_utf8_lossy(&data));
                }
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    if depth == 0 {
                        return Ok(out.trim().to_owned());
                    }
                    depth -= 1;
                }
                Event::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        element: "text".to_owned(),
                    });
                }
                _ => {}
            }
        }
    }

    /// Reads text into `slot`, as an owned `Some`.
    pub(crate) fn text_into(&mut self, slot: &mut Option<String>) -> Result<bool, ParseError> {
        *slot = Some(self.text()?);
        Ok(true)
    }

    pub(crate) fn bool_into(&mut self, slot: &mut Option<bool>) -> Result<bool, ParseError> {
        let text = self.text()?;
        *slot = Some(parse_bool(&text));
        Ok(true)
    }

    /// Reads a wrapper element whose children are all `item`, parsing each.
    pub(crate) fn list<T, F>(
        &mut self,
        context: &str,
        item: &[u8],
        mut parse_item: F,
    ) -> Result<Vec<T>, ParseError>
    where
        F: FnMut(&mut Self) -> Result<T, ParseError>,
    {
        let mut out = Vec::new();
        self.children(context, |parser, name| {
            if name == item {
                out.push(parse_item(parser)?);
                Ok(true)
            } else {
                Ok(false)
            }
        })?;
        Ok(out)
    }

    /// Reads a wrapper element whose children are all text values.
    pub(crate) fn text_list(
        &mut self,
        context: &str,
        item: &[u8],
    ) -> Result<Vec<String>, ParseError> {
        self.list(context, item, |parser| parser.text())
    }

    pub(crate) fn parse_project(&mut self) -> Result<Model, ParseError> {
        let mut model = Model::default();
        self.children("project", |parser, name| match name {
            b"modelVersion" => parser.text_into(&mut model.model_version),
            b"parent" => {
                model.parent = Some(parser.parse_parent()?);
                Ok(true)
            }
            b"groupId" => parser.text_into(&mut model.group_id),
            b"artifactId" => parser.text_into(&mut model.artifact_id),
            b"version" => parser.text_into(&mut model.version),
            b"packaging" => parser.text_into(&mut model.packaging),
            b"name" => parser.text_into(&mut model.name),
            b"description" => parser.text_into(&mut model.description),
            b"url" => parser.text_into(&mut model.url),
            b"inceptionYear" => parser.text_into(&mut model.inception_year),
            b"prerequisites" => {
                model.prerequisites = Some(parser.parse_prerequisites()?);
                Ok(true)
            }
            b"properties" => {
                parser.parse_properties(&mut model.properties)?;
                Ok(true)
            }
            b"dependencies" => {
                let parsed = parser.parse_dependencies()?;
                parser.replace_repeated("dependencies", &mut model.dependencies, parsed);
                Ok(true)
            }
            b"dependencyManagement" => {
                let parsed = parser.parse_dependency_management()?;
                parser.replace_repeated(
                    "dependencyManagement",
                    &mut model.dependency_management,
                    parsed,
                );
                Ok(true)
            }
            // Maven 4 renamed <modules> to <subprojects>, and renamed the item
            // element with it. Both are accepted; a POM using both contributes
            // from each, which Maven 4 rejects but Maven 3.9 never sees.
            b"modules" => {
                model
                    .modules
                    .extend(parser.text_list("modules", b"module")?);
                Ok(true)
            }
            b"subprojects" => {
                model
                    .modules
                    .extend(parser.text_list("subprojects", b"subproject")?);
                Ok(true)
            }
            b"build" => {
                model.build = Some(parser.parse_build("build")?);
                Ok(true)
            }
            b"profiles" => {
                let parsed = parser.list("profiles", b"profile", XmlParser::parse_profile)?;
                parser.replace_repeated("profiles", &mut model.profiles, parsed);
                Ok(true)
            }
            b"repositories" => {
                let parsed =
                    parser.list("repositories", b"repository", XmlParser::parse_repository)?;
                parser.replace_repeated("repositories", &mut model.repositories, parsed);
                Ok(true)
            }
            b"pluginRepositories" => {
                let parsed = parser.list(
                    "pluginRepositories",
                    b"pluginRepository",
                    XmlParser::parse_repository,
                )?;
                parser.replace_repeated(
                    "pluginRepositories",
                    &mut model.plugin_repositories,
                    parsed,
                );
                Ok(true)
            }
            b"distributionManagement" => {
                model.distribution_management = Some(parser.parse_distribution_management()?);
                Ok(true)
            }
            _ => Ok(false),
        })?;
        Ok(model)
    }

    fn parse_parent(&mut self) -> Result<Parent, ParseError> {
        let mut parent = Parent::default();
        self.children("parent", |parser, name| match name {
            b"groupId" => parser.text_into(&mut parent.group_id),
            b"artifactId" => parser.text_into(&mut parent.artifact_id),
            b"version" => parser.text_into(&mut parent.version),
            b"relativePath" => parser.text_into(&mut parent.relative_path),
            _ => Ok(false),
        })?;
        Ok(parent)
    }

    pub(crate) fn parse_properties(
        &mut self,
        properties: &mut crate::model::Properties,
    ) -> Result<(), ParseError> {
        self.children("properties", |parser, name| {
            let key = String::from_utf8_lossy(name).into_owned();
            let value = parser.text()?;
            properties.insert(key, value);
            Ok(true)
        })
    }

    fn parse_dependencies(&mut self) -> Result<Vec<Dependency>, ParseError> {
        self.list("dependencies", b"dependency", XmlParser::parse_dependency)
    }

    fn parse_dependency_management(&mut self) -> Result<Vec<Dependency>, ParseError> {
        let mut managed = Vec::new();
        self.children("dependencyManagement", |parser, name| {
            if name == b"dependencies" {
                managed = parser.parse_dependencies()?;
                Ok(true)
            } else {
                Ok(false)
            }
        })?;
        Ok(managed)
    }

    fn parse_dependency(&mut self) -> Result<Dependency, ParseError> {
        let mut dependency = Dependency::default();
        let mut group_id = None;
        let mut artifact_id = None;
        self.children("dependency", |parser, name| match name {
            b"groupId" => parser.text_into(&mut group_id),
            b"artifactId" => parser.text_into(&mut artifact_id),
            b"version" => parser.text_into(&mut dependency.version),
            b"type" => parser.text_into(&mut dependency.type_),
            b"classifier" => parser.text_into(&mut dependency.classifier),
            b"scope" => {
                let raw = parser.text()?;
                // An empty `<scope/>` is absent, not unrecognized. Maven's
                // validator returns early on a blank value, and warning about it
                // would put noise in the channel real problems arrive on.
                if raw.trim().is_empty() {
                    return Ok(true);
                }
                match raw.parse::<Scope>() {
                    Ok(scope) => dependency.scope = Some(scope),
                    Err(_) if crate::scope::is_maven_4_scope(&raw) => {
                        parser.warn(format!(
                            "{raw:?} is a Maven 4 scope; jv follows Maven 3.9, \
                             which treats it as compile"
                        ));
                    }
                    Err(_) => {
                        // Maven reports this as a warning and carries on, at
                        // which point the dependency behaves like compile scope.
                        parser.warn(format!(
                            "unrecognized dependency scope {raw:?}; treating as compile"
                        ));
                    }
                }
                Ok(true)
            }
            b"optional" => parser.bool_into(&mut dependency.optional),
            b"systemPath" => parser.text_into(&mut dependency.system_path),
            b"exclusions" => {
                dependency.exclusions =
                    parser.list("exclusions", b"exclusion", XmlParser::parse_exclusion)?;
                Ok(true)
            }
            _ => Ok(false),
        })?;
        dependency.group_id = group_id.unwrap_or_default();
        dependency.artifact_id = artifact_id.unwrap_or_default();
        Ok(dependency)
    }

    fn parse_exclusion(&mut self) -> Result<Exclusion, ParseError> {
        let mut group_id = None;
        let mut artifact_id = None;
        self.children("exclusion", |parser, name| match name {
            b"groupId" => parser.text_into(&mut group_id),
            b"artifactId" => parser.text_into(&mut artifact_id),
            _ => Ok(false),
        })?;
        Ok(Exclusion {
            group_id: group_id.unwrap_or_default(),
            artifact_id: artifact_id.unwrap_or_default(),
        })
    }

    fn parse_build(&mut self, context: &str) -> Result<Build, ParseError> {
        let mut build = Build::default();
        self.children(context, |parser, name| match name {
            b"sourceDirectory" => parser.text_into(&mut build.source_directory),
            b"scriptSourceDirectory" => parser.text_into(&mut build.script_source_directory),
            b"testSourceDirectory" => parser.text_into(&mut build.test_source_directory),
            b"outputDirectory" => parser.text_into(&mut build.output_directory),
            b"testOutputDirectory" => parser.text_into(&mut build.test_output_directory),
            b"directory" => parser.text_into(&mut build.directory),
            b"finalName" => parser.text_into(&mut build.final_name),
            b"defaultGoal" => parser.text_into(&mut build.default_goal),
            b"plugins" => {
                build.plugins = parser.list("plugins", b"plugin", XmlParser::parse_plugin)?;
                Ok(true)
            }
            b"pluginManagement" => {
                build.plugin_management = parser.parse_plugin_management()?;
                Ok(true)
            }
            b"extensions" => {
                build.extensions =
                    parser.list("extensions", b"extension", XmlParser::parse_extension)?;
                Ok(true)
            }
            _ => Ok(false),
        })?;
        Ok(build)
    }

    fn parse_plugin_management(&mut self) -> Result<Vec<Plugin>, ParseError> {
        let mut managed = Vec::new();
        self.children("pluginManagement", |parser, name| {
            if name == b"plugins" {
                managed = parser.list("plugins", b"plugin", XmlParser::parse_plugin)?;
                Ok(true)
            } else {
                Ok(false)
            }
        })?;
        Ok(managed)
    }

    fn parse_plugin(&mut self) -> Result<Plugin, ParseError> {
        let mut plugin = Plugin::default();
        self.children("plugin", |parser, name| match name {
            b"groupId" => parser.text_into(&mut plugin.group_id),
            b"artifactId" => parser.text_into(&mut plugin.artifact_id),
            b"version" => parser.text_into(&mut plugin.version),
            b"extensions" => parser.bool_into(&mut plugin.extensions),
            b"inherited" => parser.bool_into(&mut plugin.inherited),
            b"dependencies" => {
                plugin.dependencies = parser.parse_dependencies()?;
                Ok(true)
            }
            b"executions" => {
                plugin.executions = parser.list(
                    "executions",
                    b"execution",
                    XmlParser::parse_plugin_execution,
                )?;
                Ok(true)
            }
            // <configuration> is skipped: nothing jv does depends on it, and it
            // is the bulk of a real POM's bytes.
            _ => Ok(false),
        })?;
        Ok(plugin)
    }

    pub(crate) fn parse_plugin_execution(&mut self) -> Result<PluginExecution, ParseError> {
        let mut execution = PluginExecution::default();
        self.children("execution", |parser, name| match name {
            b"id" => parser.text_into(&mut execution.id),
            b"phase" => parser.text_into(&mut execution.phase),
            b"inherited" => parser.bool_into(&mut execution.inherited),
            b"goals" => {
                execution.goals = parser.text_list("goals", b"goal")?;
                Ok(true)
            }
            _ => Ok(false),
        })?;
        Ok(execution)
    }

    fn parse_extension(&mut self) -> Result<Extension, ParseError> {
        let mut extension = Extension::default();
        self.children("extension", |parser, name| match name {
            b"groupId" => parser.text_into(&mut extension.group_id),
            b"artifactId" => parser.text_into(&mut extension.artifact_id),
            b"version" => parser.text_into(&mut extension.version),
            _ => Ok(false),
        })?;
        Ok(extension)
    }

    fn parse_profile(&mut self) -> Result<Profile, ParseError> {
        let mut profile = Profile::default();
        self.children("profile", |parser, name| match name {
            b"id" => parser.text_into(&mut profile.id),
            b"activation" => {
                profile.activation = Some(parser.parse_activation()?);
                Ok(true)
            }
            b"properties" => {
                parser.parse_properties(&mut profile.properties)?;
                Ok(true)
            }
            b"dependencies" => {
                profile.dependencies = parser.parse_dependencies()?;
                Ok(true)
            }
            b"dependencyManagement" => {
                profile.dependency_management = parser.parse_dependency_management()?;
                Ok(true)
            }
            b"modules" => {
                profile
                    .modules
                    .extend(parser.text_list("modules", b"module")?);
                Ok(true)
            }
            b"subprojects" => {
                profile
                    .modules
                    .extend(parser.text_list("subprojects", b"subproject")?);
                Ok(true)
            }
            b"build" => {
                profile.build = Some(parser.parse_build("build")?);
                Ok(true)
            }
            b"repositories" => {
                profile.repositories =
                    parser.list("repositories", b"repository", XmlParser::parse_repository)?;
                Ok(true)
            }
            b"pluginRepositories" => {
                profile.plugin_repositories = parser.list(
                    "pluginRepositories",
                    b"pluginRepository",
                    XmlParser::parse_repository,
                )?;
                Ok(true)
            }
            b"distributionManagement" => {
                profile.distribution_management = Some(parser.parse_distribution_management()?);
                Ok(true)
            }
            _ => Ok(false),
        })?;
        Ok(profile)
    }

    pub(crate) fn parse_activation(&mut self) -> Result<Activation, ParseError> {
        let mut activation = Activation::default();
        self.children("activation", |parser, name| match name {
            b"activeByDefault" => parser.bool_into(&mut activation.active_by_default),
            b"jdk" => parser.text_into(&mut activation.jdk),
            b"os" => {
                activation.os = Some(parser.parse_activation_os()?);
                Ok(true)
            }
            b"property" => {
                activation.property = Some(parser.parse_activation_property()?);
                Ok(true)
            }
            b"file" => {
                activation.file = Some(parser.parse_activation_file()?);
                Ok(true)
            }
            b"packaging" => parser.text_into(&mut activation.packaging),
            b"condition" => parser.text_into(&mut activation.condition),
            _ => Ok(false),
        })?;
        Ok(activation)
    }

    fn parse_activation_os(&mut self) -> Result<ActivationOs, ParseError> {
        let mut os = ActivationOs::default();
        self.children("os", |parser, name| match name {
            b"name" => parser.text_into(&mut os.name),
            b"family" => parser.text_into(&mut os.family),
            b"arch" => parser.text_into(&mut os.arch),
            b"version" => parser.text_into(&mut os.version),
            _ => Ok(false),
        })?;
        Ok(os)
    }

    fn parse_activation_property(&mut self) -> Result<ActivationProperty, ParseError> {
        let mut property = ActivationProperty::default();
        self.children("property", |parser, name| match name {
            b"name" => parser.text_into(&mut property.name),
            b"value" => parser.text_into(&mut property.value),
            _ => Ok(false),
        })?;
        Ok(property)
    }

    fn parse_activation_file(&mut self) -> Result<ActivationFile, ParseError> {
        let mut file = ActivationFile::default();
        self.children("file", |parser, name| match name {
            b"missing" => parser.text_into(&mut file.missing),
            b"exists" => parser.text_into(&mut file.exists),
            _ => Ok(false),
        })?;
        Ok(file)
    }

    pub(crate) fn parse_repository(&mut self) -> Result<Repository, ParseError> {
        let mut repository = Repository::default();
        self.children("repository", |parser, name| match name {
            b"id" => parser.text_into(&mut repository.id),
            b"name" => parser.text_into(&mut repository.name),
            b"url" => parser.text_into(&mut repository.url),
            b"layout" => parser.text_into(&mut repository.layout),
            b"releases" => {
                repository.releases = Some(parser.parse_repository_policy("releases")?);
                Ok(true)
            }
            b"snapshots" => {
                repository.snapshots = Some(parser.parse_repository_policy("snapshots")?);
                Ok(true)
            }
            _ => Ok(false),
        })?;
        Ok(repository)
    }

    pub(crate) fn parse_repository_policy(
        &mut self,
        context: &str,
    ) -> Result<RepositoryPolicy, ParseError> {
        let mut policy = RepositoryPolicy::default();
        self.children(context, |parser, name| match name {
            b"enabled" => parser.bool_into(&mut policy.enabled),
            b"updatePolicy" => parser.text_into(&mut policy.update_policy),
            b"checksumPolicy" => parser.text_into(&mut policy.checksum_policy),
            _ => Ok(false),
        })?;
        Ok(policy)
    }

    fn parse_distribution_management(&mut self) -> Result<DistributionManagement, ParseError> {
        let mut management = DistributionManagement::default();
        self.children("distributionManagement", |parser, name| match name {
            b"relocation" => {
                management.relocation = Some(parser.parse_relocation()?);
                Ok(true)
            }
            b"status" => parser.text_into(&mut management.status),
            _ => Ok(false),
        })?;
        Ok(management)
    }

    fn parse_prerequisites(&mut self) -> Result<Prerequisites, ParseError> {
        let mut prerequisites = Prerequisites::default();
        self.children("prerequisites", |parser, name| match name {
            b"maven" => parser.text_into(&mut prerequisites.maven),
            _ => Ok(false),
        })?;
        Ok(prerequisites)
    }

    fn parse_relocation(&mut self) -> Result<Relocation, ParseError> {
        let mut relocation = Relocation::default();
        self.children("relocation", |parser, name| match name {
            b"groupId" => parser.text_into(&mut relocation.group_id),
            b"artifactId" => parser.text_into(&mut relocation.artifact_id),
            b"version" => parser.text_into(&mut relocation.version),
            b"message" => parser.text_into(&mut relocation.message),
            _ => Ok(false),
        })?;
        Ok(relocation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_wrapper_is_reported_rather_than_silently_dropped() {
        // Maven's own reader rejects a duplicated tag outright, so a POM like
        // this is one Maven will not read at all. Resolving it silently — with
        // half its dependencies gone — is the worst available answer.
        let parsed = parse_pom(
            r#"<project>
                 <dependencies><dependency><groupId>g</groupId>
                   <artifactId>first</artifactId></dependency></dependencies>
                 <dependencies><dependency><groupId>g</groupId>
                   <artifactId>second</artifactId></dependency></dependencies>
               </project>"#,
        )
        .unwrap();
        assert_eq!(parsed.model.dependencies.len(), 1);
        assert_eq!(parsed.model.dependencies[0].artifact_id, "second");
        assert!(
            parsed.warnings.iter().any(|w| w.contains("<dependencies>")),
            "expected a warning, got {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn a_wrapper_that_appears_once_warns_about_nothing() {
        let parsed = parse_pom(
            r#"<project><dependencies><dependency><groupId>g</groupId>
                 <artifactId>a</artifactId></dependency></dependencies></project>"#,
        )
        .unwrap();
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    #[test]
    fn an_empty_scope_is_absent_rather_than_unrecognized() {
        // Maven's validator returns early on a blank value. Warning here would
        // put noise in the channel real problems arrive on.
        let parsed = parse_pom(
            r#"<project><dependencies><dependency>
                 <groupId>g</groupId><artifactId>a</artifactId><scope/>
               </dependency></dependencies></project>"#,
        )
        .unwrap();
        assert_eq!(parsed.model.dependencies[0].scope, None);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    fn parse(xml: &str) -> ParsedPom {
        parse_pom(xml).expect("parse")
    }

    #[test]
    fn minimal_pom() {
        let pom = parse(
            r#"<project>
                 <modelVersion>4.0.0</modelVersion>
                 <groupId>g</groupId>
                 <artifactId>a</artifactId>
                 <version>1.0</version>
               </project>"#,
        );
        assert_eq!(pom.model.model_version.as_deref(), Some("4.0.0"));
        assert_eq!(pom.model.group_id.as_deref(), Some("g"));
        assert_eq!(pom.model.packaging, None);
        assert_eq!(pom.model.packaging_or_default(), "jar");
        assert!(pom.warnings.is_empty());
    }

    #[test]
    fn namespaced_and_prefixed_elements() {
        // The default namespace is the common case; a prefix is rare but legal.
        let pom = parse(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
                 <artifactId>a</artifactId>
               </project>"#,
        );
        assert_eq!(pom.model.artifact_id.as_deref(), Some("a"));

        let prefixed = parse(
            r#"<m:project xmlns:m="http://maven.apache.org/POM/4.0.0">
                 <m:artifactId>a</m:artifactId>
               </m:project>"#,
        );
        assert_eq!(prefixed.model.artifact_id.as_deref(), Some("a"));
    }

    #[test]
    fn unknown_elements_and_configuration_are_skipped() {
        let pom = parse(
            r#"<project>
                 <artifactId>a</artifactId>
                 <someFutureElement><nested attr="x">text</nested></someFutureElement>
                 <build>
                   <plugins>
                     <plugin>
                       <artifactId>maven-surefire-plugin</artifactId>
                       <configuration>
                         <artifactId>not-a-plugin-id</artifactId>
                         <systemProperties><property><name>n</name></property></systemProperties>
                       </configuration>
                     </plugin>
                   </plugins>
                 </build>
               </project>"#,
        );
        assert_eq!(pom.model.artifact_id.as_deref(), Some("a"));
        let plugins = &pom.model.build.as_ref().unwrap().plugins;
        assert_eq!(plugins.len(), 1);
        // The artifactId inside <configuration> must not leak into the plugin.
        assert_eq!(
            plugins[0].artifact_id.as_deref(),
            Some("maven-surefire-plugin")
        );
    }

    #[test]
    fn dependency_fields_and_exclusions() {
        let pom = parse(
            r#"<project>
                 <dependencies>
                   <dependency>
                     <groupId>g</groupId>
                     <artifactId>a</artifactId>
                     <version>[1.0,2.0)</version>
                     <type>test-jar</type>
                     <classifier>tests</classifier>
                     <scope>test</scope>
                     <optional>true</optional>
                     <exclusions>
                       <exclusion><groupId>x</groupId><artifactId>*</artifactId></exclusion>
                     </exclusions>
                   </dependency>
                 </dependencies>
               </project>"#,
        );
        let dep = &pom.model.dependencies[0];
        assert_eq!(dep.version.as_deref(), Some("[1.0,2.0)"));
        assert_eq!(dep.type_.as_deref(), Some("test-jar"));
        assert_eq!(dep.classifier.as_deref(), Some("tests"));
        assert_eq!(dep.scope, Some(Scope::Test));
        assert!(dep.is_optional());
        assert_eq!(dep.exclusions.len(), 1);
        assert!(dep.exclusions[0].matches("x", "anything"));
    }

    #[test]
    fn self_closing_and_empty_elements() {
        let pom = parse(
            r#"<project>
                 <artifactId/>
                 <dependencies/>
                 <dependencies>
                   <dependency>
                     <groupId>g</groupId><artifactId>a</artifactId>
                     <optional/>
                   </dependency>
                 </dependencies>
               </project>"#,
        );
        assert_eq!(pom.model.artifact_id.as_deref(), Some(""));
        // An empty <optional/> reads as false, matching Boolean.parseBoolean("").
        assert_eq!(pom.model.dependencies[0].optional, Some(false));
    }

    #[test]
    fn unknown_scope_warns_instead_of_failing() {
        let pom = parse(
            r#"<project><dependencies><dependency>
                 <groupId>g</groupId><artifactId>a</artifactId><scope>Compile</scope>
               </dependency></dependencies></project>"#,
        );
        assert_eq!(pom.model.dependencies[0].scope, None);
        assert_eq!(pom.model.dependencies[0].scope_or_default(), Scope::Compile);
        assert_eq!(pom.warnings.len(), 1);
        assert!(pom.warnings[0].contains("Compile"));
    }

    #[test]
    fn dependency_management_and_bom_import() {
        let pom = parse(
            r#"<project>
                 <dependencyManagement><dependencies>
                   <dependency>
                     <groupId>org.springframework.boot</groupId>
                     <artifactId>spring-boot-dependencies</artifactId>
                     <version>3.2.0</version>
                     <type>pom</type>
                     <scope>import</scope>
                   </dependency>
                 </dependencies></dependencyManagement>
               </project>"#,
        );
        let managed = &pom.model.dependency_management[0];
        assert_eq!(managed.scope, Some(Scope::Import));
        assert_eq!(managed.type_.as_deref(), Some("pom"));
        assert!(pom.model.dependencies.is_empty());
    }

    #[test]
    fn text_is_trimmed_and_entities_unescaped() {
        let pom = parse(
            r#"<project>
                 <name>
                   Demo &amp; Friends
                 </name>
                 <description><![CDATA[raw <text> here]]></description>
               </project>"#,
        );
        assert_eq!(pom.model.name.as_deref(), Some("Demo & Friends"));
        assert_eq!(pom.model.description.as_deref(), Some("raw <text> here"));
    }

    #[test]
    fn properties_are_captured_verbatim() {
        let pom = parse(
            r#"<project><properties>
                 <java.version>17</java.version>
                 <my.custom.flag>true</my.custom.flag>
                 <empty/>
               </properties></project>"#,
        );
        assert_eq!(pom.model.properties.get("java.version").unwrap(), "17");
        assert_eq!(pom.model.properties.get("my.custom.flag").unwrap(), "true");
        assert_eq!(pom.model.properties.get("empty").unwrap(), "");
    }

    #[test]
    fn modules_accept_both_spellings() {
        // Maven 4 renamed the wrapper *and* the item element, so <subprojects>
        // holds <subproject>, not <module>.
        let pom = parse(
            r#"<project>
                 <modules><module>a</module><module>b</module></modules>
                 <subprojects><subproject>c</subproject></subprojects>
               </project>"#,
        );
        assert_eq!(pom.model.modules, vec!["a", "b", "c"]);
    }

    #[test]
    fn subprojects_ignores_the_wrong_item_element() {
        let pom = parse(r#"<project><subprojects><module>c</module></subprojects></project>"#);
        assert!(pom.model.modules.is_empty());
    }

    #[test]
    fn profiles_with_every_activator() {
        let pom = parse(
            r#"<project><profiles>
                 <profile>
                   <id>p1</id>
                   <activation>
                     <activeByDefault>true</activeByDefault>
                     <jdk>[11,)</jdk>
                     <os><family>unix</family><arch>aarch64</arch></os>
                     <property><name>!skipIt</name></property>
                     <file><exists>${basedir}/marker</exists></file>
                   </activation>
                   <properties><p>v</p></properties>
                   <dependencies><dependency>
                     <groupId>g</groupId><artifactId>a</artifactId><version>1</version>
                   </dependency></dependencies>
                 </profile>
               </profiles></project>"#,
        );
        let profile = &pom.model.profiles[0];
        assert_eq!(profile.id.as_deref(), Some("p1"));
        let activation = profile.activation.as_ref().unwrap();
        assert_eq!(activation.active_by_default, Some(true));
        assert_eq!(activation.jdk.as_deref(), Some("[11,)"));
        assert_eq!(
            activation.os.as_ref().unwrap().family.as_deref(),
            Some("unix")
        );
        assert_eq!(
            activation.property.as_ref().unwrap().name.as_deref(),
            Some("!skipIt")
        );
        assert_eq!(
            activation.file.as_ref().unwrap().exists.as_deref(),
            Some("${basedir}/marker")
        );
        assert!(activation.has_conditions());
        assert_eq!(profile.dependencies.len(), 1);
    }

    #[test]
    fn repositories_with_policies() {
        let pom = parse(
            r#"<project><repositories>
                 <repository>
                   <id>central</id>
                   <url>https://repo.maven.apache.org/maven2</url>
                   <releases><enabled>true</enabled><updatePolicy>never</updatePolicy></releases>
                   <snapshots><enabled>false</enabled><checksumPolicy>fail</checksumPolicy></snapshots>
                 </repository>
               </repositories></project>"#,
        );
        let repo = &pom.model.repositories[0];
        assert_eq!(repo.id.as_deref(), Some("central"));
        let releases = repo.releases.as_ref().unwrap();
        assert_eq!(releases.enabled, Some(true));
        assert_eq!(releases.update_policy.as_deref(), Some("never"));
        let snapshots = repo.snapshots.as_ref().unwrap();
        assert_eq!(snapshots.enabled, Some(false));
        assert_eq!(snapshots.checksum_policy.as_deref(), Some("fail"));
    }

    #[test]
    fn relocation_is_captured() {
        let pom = parse(
            r#"<project><distributionManagement>
                 <relocation>
                   <groupId>new.group</groupId>
                   <message>moved</message>
                 </relocation>
                 <status>deployed</status>
               </distributionManagement></project>"#,
        );
        let management = pom.model.distribution_management.as_ref().unwrap();
        let relocation = management.relocation.as_ref().unwrap();
        assert_eq!(relocation.group_id.as_deref(), Some("new.group"));
        // Absent fields mean "unchanged", not "empty".
        assert_eq!(relocation.artifact_id, None);
        assert_eq!(management.status.as_deref(), Some("deployed"));
    }

    #[test]
    fn comments_declaration_and_doctype_are_ignored() {
        let pom = parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
               <!-- a comment -->
               <project>
                 <!-- another -->
                 <artifactId>a</artifactId>
               </project>"#,
        );
        assert_eq!(pom.model.artifact_id.as_deref(), Some("a"));
    }

    #[test]
    fn non_project_root_is_rejected() {
        let error = parse_pom("<settings><offline>true</offline></settings>").unwrap_err();
        assert!(matches!(error, ParseError::UnexpectedRoot { .. }));
    }

    #[test]
    fn empty_document_is_rejected() {
        assert!(matches!(parse_pom(""), Err(ParseError::Empty)));
    }

    #[test]
    fn mismatched_tags_are_rejected() {
        assert!(parse_pom("<project><artifactId>a</version></project>").is_err());
    }

    #[test]
    fn plugin_dependencies_are_captured() {
        let pom = parse(
            r#"<project><build>
                 <pluginManagement><plugins><plugin>
                   <artifactId>maven-compiler-plugin</artifactId>
                   <version>3.13.0</version>
                 </plugin></plugins></pluginManagement>
                 <plugins><plugin>
                   <groupId>org.codehaus.mojo</groupId>
                   <artifactId>exec-maven-plugin</artifactId>
                   <extensions>true</extensions>
                   <dependencies><dependency>
                     <groupId>g</groupId><artifactId>a</artifactId><version>1</version>
                   </dependency></dependencies>
                 </plugin></plugins>
                 <extensions><extension>
                   <groupId>e</groupId><artifactId>ext</artifactId><version>2</version>
                 </extension></extensions>
               </build></project>"#,
        );
        let build = pom.model.build.as_ref().unwrap();
        assert_eq!(build.plugin_management.len(), 1);
        assert_eq!(
            build.plugin_management[0].version.as_deref(),
            Some("3.13.0")
        );
        assert_eq!(build.plugins[0].extensions, Some(true));
        assert_eq!(build.plugins[0].dependencies.len(), 1);
        assert_eq!(build.extensions[0].artifact_id.as_deref(), Some("ext"));
    }
}
