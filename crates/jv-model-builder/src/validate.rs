//! Checking that coordinates are coordinates.
//!
//! Ported from `DefaultModelValidator.validateEffectiveModel`. Maven restricts
//! `groupId` and `artifactId` to `[A-Za-z0-9._-]` and refuses `.` and `..`
//! outright; a version may hold more, but not the characters that mean something
//! to a filesystem.
//!
//! # Why this is not merely tidiness
//!
//! Everything downstream assumes these rules hold, because upstream's validator
//! guarantees them before any of that code is reached. jv had no validator, so
//! the same code was reached with values Maven would have refused:
//!
//! * A coordinate becomes a path. `<groupId>/tmp/anywhere</groupId>` produced an
//!   absolute one, and `Path::join` discards its base when given an absolute
//!   path. (`jv-repo`'s layout sanitizes independently, so this is the second of
//!   two locks on that door — but the first lock was fitted after the door was
//!   found open.)
//! * A coordinate becomes a label in `dot`, `graphml`, `tgf` and `json`, none of
//!   which escape anything, because the upstream visitors they are ported from
//!   do not. That is safe for Maven exactly because the validator ran first. An
//!   `artifactId` containing a quote or a newline forged a node in the output.
//!
//! # What jv does about an invalid coordinate
//!
//! Maven fails the build. jv reports the same problem at the same severity but
//! *drops* the offending dependency and carries on, which is a deliberate
//! divergence: a resolve is worth completing even when one entry in one POM
//! three levels down is malformed, and dropping is enough to keep the value out
//! of every path and every renderer. The project's own coordinates are a
//! different matter — those are fatal, because there is nothing left to resolve.

use jv_model::{Dependency, Model};

use crate::problem::Problem;

/// Whether a `groupId` or `artifactId` is one Maven would accept.
///
/// `.` and `..` pass the character test and are still refused: the repository
/// layout uses ids verbatim as directory names, so they name the wrong
/// directory rather than a coordinate.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Whether a version is one Maven would accept.
///
/// A ban list rather than an allow list, matching `ILLEGAL_VERSION_CHARS`:
/// versions in the wild carry `+`, `~` and worse, and only the characters that
/// mean something to a filesystem are refused.
pub fn is_valid_version(version: &str) -> bool {
    !version.is_empty()
        && version != "."
        && version != ".."
        && !version.contains(['\\', '/', ':', '"', '<', '>', '|', '?', '*'])
        // Not upstream's, and deliberately stricter: a control character in a
        // version reaches a renderer that does not escape it, where a newline
        // forges a line of output.
        && !version.chars().any(char::is_control)
}

/// Validates a model's own coordinates and drops any dependency whose are
/// invalid.
///
/// Returns the problems found. The model is left with only dependencies safe to
/// turn into paths and labels.
pub fn validate(model: &mut Model, source: &str) -> Vec<Problem> {
    let mut problems = Vec::new();

    for (field, value) in [
        ("groupId", model.group_id.as_deref()),
        ("artifactId", model.artifact_id.as_deref()),
    ] {
        if let Some(value) = value {
            if !is_valid_id(value) {
                problems.push(Problem::error(
                    source,
                    format!("{field} with value '{value}' does not match a valid id pattern."),
                ));
            }
        }
    }
    if let Some(version) = model.version.as_deref() {
        if !is_valid_version(version) {
            problems.push(Problem::error(
                source,
                format!("version with value '{version}' does not match a valid version pattern."),
            ));
        }
    }

    let mut kept = Vec::with_capacity(model.dependencies.len());
    for dependency in std::mem::take(&mut model.dependencies) {
        match invalid_field(&dependency) {
            Some(complaint) => problems.push(Problem::error(
                source,
                format!(
                    "dependencies.dependency.{complaint} (for {}:{}); it was dropped",
                    dependency.group_id, dependency.artifact_id
                ),
            )),
            None => kept.push(dependency),
        }
    }
    model.dependencies = kept;

    problems
}

/// The first field of a dependency that is not a valid coordinate.
fn invalid_field(dependency: &Dependency) -> Option<String> {
    if !is_valid_id(&dependency.group_id) {
        return Some(format!(
            "groupId with value '{}' does not match a valid id pattern.",
            dependency.group_id
        ));
    }
    if !is_valid_id(&dependency.artifact_id) {
        return Some(format!(
            "artifactId with value '{}' does not match a valid id pattern.",
            dependency.artifact_id
        ));
    }
    // A version is only checked when the declaration states one: an absent
    // version is management's job to supply, and complaining here would fire on
    // every dependency governed by a BOM.
    if let Some(version) = dependency.version.as_deref() {
        // A range is a version *expression*, not a version, and carries
        // characters an id may not.
        let is_range = version.starts_with('[') || version.starts_with('(');
        if !is_range && !is_valid_version(version) {
            return Some(format!(
                "version with value '{version}' does not match a valid version pattern."
            ));
        }
    }
    if let Some(classifier) = dependency.classifier.as_deref() {
        if !classifier.is_empty() && !is_valid_id(classifier) {
            return Some(format!(
                "classifier with value '{classifier}' does not match a valid id pattern."
            ));
        }
    }
    if let Some(type_) = dependency.type_.as_deref() {
        if !type_.is_empty() && !is_valid_id(type_) {
            return Some(format!(
                "type with value '{type_}' does not match a valid id pattern."
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(group_id: &str, artifact_id: &str, version: Option<&str>) -> Dependency {
        Dependency {
            group_id: group_id.to_owned(),
            artifact_id: artifact_id.to_owned(),
            version: version.map(str::to_owned),
            ..Dependency::default()
        }
    }

    #[test]
    fn ordinary_coordinates_are_accepted() {
        // The check must cost nothing real, or it changes what every project
        // resolves.
        for id in [
            "org.springframework.boot",
            "spring-boot-starter-web",
            "a_b",
            "a.b.c",
            "1",
        ] {
            assert!(is_valid_id(id), "{id} should be a valid id");
        }
        for version in [
            "1.0",
            "1.0.0-RC1",
            "3.3.0+build.5",
            "1.0-SNAPSHOT",
            "2.0~rc",
        ] {
            assert!(is_valid_version(version), "{version} should be valid");
        }
    }

    #[test]
    fn an_id_that_would_escape_its_directory_is_refused() {
        // The repository layout uses these verbatim as directory names.
        for id in ["..", ".", "/tmp/anywhere", "a/b", "a\\b", ""] {
            assert!(!is_valid_id(id), "{id:?} should be refused");
        }
    }

    #[test]
    fn an_id_that_would_forge_output_is_refused() {
        // dot, graphml, tgf and json escape nothing — faithfully, because the
        // upstream visitors do not either, which is safe for Maven only because
        // its validator ran first.
        for id in [
            "de\"mo",
            "a</y:NodeLabel><evil/>",
            "a\nb",
            "a b",
            "a;b",
            "a:b",
        ] {
            assert!(!is_valid_id(id), "{id:?} should be refused");
        }
    }

    #[test]
    fn a_control_character_in_a_version_is_refused() {
        // Upstream's version rule is a ban list that does not cover these, but a
        // newline in a version forges a line of tree output just as well as one
        // in an id.
        assert!(!is_valid_version("1.0\nforged"));
        assert!(!is_valid_version("1.0\u{0}"));
        assert!(!is_valid_version("1.0/../../etc"));
    }

    #[test]
    fn an_invalid_dependency_is_dropped_and_reported() {
        let mut model = Model {
            group_id: Some("com.example".to_owned()),
            artifact_id: Some("app".to_owned()),
            version: Some("1.0".to_owned()),
            dependencies: vec![
                dependency("org.slf4j", "slf4j-api", Some("2.0.9")),
                dependency("/tmp/anywhere", "evil", Some("1.0")),
                dependency("org.ok", "fine", None),
            ],
            ..Model::default()
        };
        let problems = validate(&mut model, "com.example:app:1.0");

        // The good ones survive, including the one whose version management will
        // supply — complaining about that would fire on every BOM-governed entry.
        assert_eq!(model.dependencies.len(), 2);
        assert_eq!(model.dependencies[0].artifact_id, "slf4j-api");
        assert_eq!(model.dependencies[1].artifact_id, "fine");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("/tmp/anywhere"));
    }

    #[test]
    fn a_version_range_is_not_mistaken_for_an_invalid_version() {
        let mut model = Model {
            dependencies: vec![dependency("g", "a", Some("[1.0,2.0)"))],
            ..Model::default()
        };
        let problems = validate(&mut model, "g:a:1");
        // A range is a version expression, and its brackets and comma are not
        // characters a plain version may carry.
        assert_eq!(model.dependencies.len(), 1);
        assert!(problems.is_empty());
    }

    #[test]
    fn the_projects_own_bad_coordinates_are_reported() {
        let mut model = Model {
            group_id: Some("com/example".to_owned()),
            artifact_id: Some("..".to_owned()),
            version: Some("1.0".to_owned()),
            ..Model::default()
        };
        let problems = validate(&mut model, "?");
        assert_eq!(problems.len(), 2);
    }
}
