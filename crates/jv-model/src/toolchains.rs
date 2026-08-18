//! `toolchains.xml`, and the requirements a build places on it.
//!
//! # What toolchains do *not* do
//!
//! They do not affect resolution. `JdkVersionProfileActivator` reads
//! `java.version` from the system properties of the JVM running Maven, and
//! nothing in model building references toolchains at all — checked against the
//! Maven sources rather than assumed. A `<jdk>`-activated profile therefore
//! resolves the same whether or not a toolchain is configured, and jv's
//! dependency graph is unaffected by this file.
//!
//! # What they do
//!
//! `mvn -o verify` *fails to build* when a project requires a toolchain the
//! machine does not provide, with an error that arrives long after `jv sync`
//! has reported success. A sync that leaves an unbuildable project is a false
//! success, so jv reads the same files Maven would and says so up front. It
//! cannot fix the problem — the missing thing is a JDK on disk, not an artifact
//! to download — but a clear warning before the build beats a confusing failure
//! during it.
//!
//! # Matching, as Maven implements it
//!
//! From `JavaToolchainFactory` and `RequirementMatcherFactory`:
//!
//! * The `version` key matches by *range*: the requirement from the POM is
//!   parsed as a version specification, and the toolchain's provided version
//!   must fall inside it. A bare requirement means exact equality.
//! * Every other key is a case-insensitive string comparison.
//! * A requirement key with no corresponding `<provides>` token fails the
//!   match, so extra requirements narrow rather than widen.
//! * A `jdk` toolchain whose `<jdkHome>` does not exist is misconfigured and
//!   Maven discards it, so an entry pointing at a JDK that has been uninstalled
//!   is the same as no entry at all.

use std::path::Path;

use jv_version::{Constraint, Version};

/// One `<toolchain>` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Toolchain {
    /// `<type>`, e.g. `jdk`.
    pub kind: String,
    /// `<provides>` tokens, in document order.
    pub provides: Vec<(String, String)>,
    /// `<configuration>` entries, in document order.
    pub configuration: Vec<(String, String)>,
}

impl Toolchain {
    /// A `<provides>` value by key.
    pub fn provided(&self, key: &str) -> Option<&str> {
        self.provides
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// A `<configuration>` value by key.
    pub fn configured(&self, key: &str) -> Option<&str> {
        self.configuration
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// `<jdkHome>`, for a `jdk` toolchain.
    pub fn jdk_home(&self) -> Option<&str> {
        self.configured("jdkHome")
    }

    /// Whether Maven would keep this entry.
    ///
    /// A `jdk` toolchain without an existing `jdkHome` throws
    /// `MisconfiguredToolchainException` and is dropped, so jv drops it too
    /// rather than reporting a match the build will not get.
    pub fn is_usable(&self) -> bool {
        if self.kind != "jdk" {
            return true;
        }
        self.jdk_home().is_some_and(|home| Path::new(home).exists())
    }

    /// Whether this toolchain satisfies every requirement.
    pub fn matches(&self, kind: &str, requirements: &[(String, String)]) -> bool {
        if self.kind != kind {
            return false;
        }
        requirements.iter().all(|(key, requirement)| {
            let Some(provided) = self.provided(key) else {
                // An unprovided key fails the match: requirements narrow.
                return false;
            };
            if key == "version" {
                // The *requirement* is the specification and the *provided*
                // value is tested against it, which is the direction that makes
                // `[11,)` in a POM accept a toolchain declaring `17`.
                Constraint::parse(requirement)
                    .is_ok_and(|constraint| constraint.contains(&Version::parse(provided)))
            } else {
                provided.eq_ignore_ascii_case(requirement)
            }
        })
    }
}

/// The contents of a `toolchains.xml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Toolchains {
    pub toolchains: Vec<Toolchain>,
}

impl Toolchains {
    /// The first usable toolchain satisfying the requirements, as Maven's
    /// selection does.
    pub fn select(&self, kind: &str, requirements: &[(String, String)]) -> Option<&Toolchain> {
        self.toolchains
            .iter()
            .filter(|toolchain| toolchain.is_usable())
            .find(|toolchain| toolchain.matches(kind, requirements))
    }

    /// Merges a lower-precedence file underneath this one.
    ///
    /// Maven reads the user file and the installation file and concatenates
    /// them, user first, so a user entry is selected before a global one.
    pub fn merge_under(mut self, global: Toolchains) -> Self {
        self.toolchains.extend(global.toolchains);
        self
    }
}

/// Reads a `toolchains.xml`.
pub fn parse_toolchains(xml: &str) -> Toolchains {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut toolchains = Vec::new();
    let mut current = Toolchain::default();
    let mut path: Vec<String> = Vec::new();
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "toolchain" && path.last().is_some_and(|last| last == "toolchains") {
                    current = Toolchain::default();
                }
                path.push(name);
            }
            Ok(quick_xml::events::Event::End(element)) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "toolchain" && !current.kind.is_empty() {
                    toolchains.push(std::mem::take(&mut current));
                }
                path.pop();
            }
            Ok(quick_xml::events::Event::Text(text)) => {
                let value = text.unescape().unwrap_or_default().trim().to_string();
                if value.is_empty() {
                    continue;
                }
                match path.as_slice() {
                    [.., parent, field] if parent == "toolchain" && field == "type" => {
                        current.kind = value;
                    }
                    [.., grandparent, parent, field]
                        if grandparent == "toolchain" && parent == "provides" =>
                    {
                        current.provides.push((field.clone(), value));
                    }
                    [.., grandparent, parent, field]
                        if grandparent == "toolchain" && parent == "configuration" =>
                    {
                        current.configuration.push((field.clone(), value));
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    Toolchains { toolchains }
}

/// A toolchain a build asks for, from `maven-toolchains-plugin` configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolchainRequirement {
    /// The toolchain type, e.g. `jdk`.
    pub kind: String,
    /// The `<provides>` keys and values the build requires.
    pub requirements: Vec<(String, String)>,
}

impl ToolchainRequirement {
    /// `jdk (version=[11,), vendor=openjdk)`, for messages.
    pub fn describe(&self) -> String {
        if self.requirements.is_empty() {
            return self.kind.clone();
        }
        let inner = self
            .requirements
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ({inner})", self.kind)
    }
}

/// The toolchains a POM requires, from `maven-toolchains-plugin`.
///
/// Read straight out of the POM text rather than from the model, because jv's
/// `Plugin` deliberately does not carry `<configuration>` — general
/// configuration merging (with `combine.children` and `combine.self`) is a
/// large feature that jv does not need, and half of it would be worse than
/// none. This reads only the one plugin's `<toolchains>` block, which has a
/// fixed two-level shape.
pub fn required_toolchains(pom_xml: &str) -> Vec<ToolchainRequirement> {
    let mut reader = quick_xml::Reader::from_str(pom_xml);
    reader.config_mut().trim_text(true);

    let mut found: Vec<ToolchainRequirement> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut buffer = Vec::new();

    // Depth of the `<plugin>` element for the toolchains plugin, so nested
    // plugins cannot be confused for it.
    let mut plugin_depth: Option<usize> = None;
    let mut artifact_id: Option<String> = None;
    let mut current: Option<ToolchainRequirement> = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                path.push(name.clone());

                if name == "plugin" && plugin_depth.is_none() {
                    artifact_id = None;
                }
                // A child of `<toolchains>` inside the plugin opens a
                // requirement, and its name is the toolchain type.
                if let Some(depth) = plugin_depth
                    && path.len() > depth
                    && path.get(path.len() - 2).is_some_and(|p| p == "toolchains")
                {
                    current = Some(ToolchainRequirement {
                        kind: name,
                        requirements: Vec::new(),
                    });
                }
            }
            Ok(quick_xml::events::Event::End(_)) => {
                if let Some(depth) = plugin_depth
                    && path.len() == depth
                {
                    plugin_depth = None;
                }
                if path
                    .get(path.len().wrapping_sub(2))
                    .is_some_and(|p| p == "toolchains")
                    && let Some(requirement) = current.take()
                    && !requirement.requirements.is_empty()
                {
                    found.push(requirement);
                }
                path.pop();
            }
            Ok(quick_xml::events::Event::Text(text)) => {
                let value = text.unescape().unwrap_or_default().trim().to_string();
                if value.is_empty() {
                    continue;
                }
                if let Some(field) = path.last() {
                    if field == "artifactId" && plugin_depth.is_none() {
                        artifact_id = Some(value.clone());
                        if value == "maven-toolchains-plugin" {
                            // The `<plugin>` element is the grandparent here.
                            plugin_depth = Some(path.len() - 1);
                        }
                    } else if let Some(requirement) = current.as_mut() {
                        requirement.requirements.push((field.clone(), value));
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    let _ = artifact_id;
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLCHAINS: &str = r#"
<toolchains>
  <toolchain>
    <type>jdk</type>
    <provides>
      <version>17</version>
      <vendor>temurin</vendor>
    </provides>
    <configuration>
      <jdkHome>/opt/jdk-17</jdkHome>
    </configuration>
  </toolchain>
  <toolchain>
    <type>jdk</type>
    <provides>
      <version>11</version>
      <vendor>openjdk</vendor>
    </provides>
    <configuration>
      <jdkHome>/opt/jdk-11</jdkHome>
    </configuration>
  </toolchain>
</toolchains>"#;

    fn requirement(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn toolchains_parse_with_provides_and_configuration() {
        let parsed = parse_toolchains(TOOLCHAINS);
        assert_eq!(parsed.toolchains.len(), 2);
        assert_eq!(parsed.toolchains[0].kind, "jdk");
        assert_eq!(parsed.toolchains[0].provided("version"), Some("17"));
        assert_eq!(parsed.toolchains[0].provided("vendor"), Some("temurin"));
        assert_eq!(parsed.toolchains[0].jdk_home(), Some("/opt/jdk-17"));
    }

    #[test]
    fn a_version_requirement_matches_by_range() {
        let parsed = parse_toolchains(TOOLCHAINS);
        let jdk17 = &parsed.toolchains[0];
        // The requirement is the specification; the provided version is tested
        // against it.
        assert!(jdk17.matches("jdk", &requirement(&[("version", "[11,)")])));
        assert!(jdk17.matches("jdk", &requirement(&[("version", "[17,18)")])));
        assert!(!jdk17.matches("jdk", &requirement(&[("version", "[8,11]")])));
    }

    #[test]
    fn a_bare_version_requirement_is_exact() {
        let parsed = parse_toolchains(TOOLCHAINS);
        let jdk17 = &parsed.toolchains[0];
        assert!(jdk17.matches("jdk", &requirement(&[("version", "17")])));
        assert!(!jdk17.matches("jdk", &requirement(&[("version", "11")])));
    }

    #[test]
    fn non_version_keys_compare_case_insensitively() {
        let parsed = parse_toolchains(TOOLCHAINS);
        let jdk17 = &parsed.toolchains[0];
        assert!(jdk17.matches("jdk", &requirement(&[("vendor", "TEMURIN")])));
        assert!(!jdk17.matches("jdk", &requirement(&[("vendor", "zulu")])));
    }

    #[test]
    fn an_unprovided_requirement_key_fails_the_match() {
        let parsed = parse_toolchains(TOOLCHAINS);
        // Requirements narrow; they never widen.
        assert!(
            !parsed.toolchains[0].matches("jdk", &requirement(&[("id", "corp-jdk")])),
            "a key the toolchain does not provide must not match"
        );
    }

    #[test]
    fn a_different_type_never_matches() {
        let parsed = parse_toolchains(TOOLCHAINS);
        assert!(!parsed.toolchains[0].matches("netbeans", &requirement(&[("version", "17")])));
    }

    #[test]
    fn selection_skips_toolchains_whose_jdk_home_is_gone() {
        // Maven discards a misconfigured entry, so a match jv reported here
        // would be a match the build does not get.
        let parsed = parse_toolchains(TOOLCHAINS);
        assert!(
            parsed
                .select("jdk", &requirement(&[("version", "[11,)")]))
                .is_none(),
            "/opt/jdk-17 does not exist in a test environment, so nothing is usable"
        );

        let directory = tempfile::tempdir().unwrap();
        let usable = parse_toolchains(&format!(
            "<toolchains><toolchain><type>jdk</type>\
             <provides><version>21</version></provides>\
             <configuration><jdkHome>{}</jdkHome></configuration>\
             </toolchain></toolchains>",
            directory.path().display()
        ));
        assert!(
            usable
                .select("jdk", &requirement(&[("version", "[21,)")]))
                .is_some()
        );
    }

    #[test]
    fn user_entries_are_selected_before_global_ones() {
        let user = parse_toolchains(
            "<toolchains><toolchain><type>x</type><provides><id>user</id></provides></toolchain></toolchains>",
        );
        let global = parse_toolchains(
            "<toolchains><toolchain><type>x</type><provides><id>global</id></provides></toolchain></toolchains>",
        );
        let merged = user.merge_under(global);
        assert_eq!(merged.toolchains.len(), 2);
        assert_eq!(merged.toolchains[0].provided("id"), Some("user"));
    }

    #[test]
    fn requirements_are_read_from_the_toolchains_plugin() {
        let requirements = required_toolchains(
            r#"<project>
  <build>
    <plugins>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-compiler-plugin</artifactId>
        <configuration><source>17</source></configuration>
      </plugin>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-toolchains-plugin</artifactId>
        <version>3.1.0</version>
        <configuration>
          <toolchains>
            <jdk>
              <version>[11,)</version>
              <vendor>temurin</vendor>
            </jdk>
          </toolchains>
        </configuration>
      </plugin>
    </plugins>
  </build>
</project>"#,
        );
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].kind, "jdk");
        assert_eq!(
            requirements[0].requirements,
            requirement(&[("version", "[11,)"), ("vendor", "temurin")])
        );
        assert_eq!(
            requirements[0].describe(),
            "jdk (version=[11,), vendor=temurin)"
        );
    }

    #[test]
    fn a_pom_without_the_plugin_requires_nothing() {
        assert!(
            required_toolchains("<project><build><plugins></plugins></build></project>").is_empty()
        );
        // Another plugin's `<configuration>` must not be mistaken for one.
        assert!(
            required_toolchains(
                r#"<project><build><plugins><plugin>
                     <artifactId>maven-surefire-plugin</artifactId>
                     <configuration><toolchains><jdk><version>17</version></jdk></toolchains></configuration>
                   </plugin></plugins></build></project>"#
            )
            .is_empty(),
            "only maven-toolchains-plugin declares toolchain requirements"
        );
    }
}
