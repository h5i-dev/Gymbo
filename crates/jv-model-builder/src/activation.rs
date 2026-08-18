//! Profile activation.
//!
//! Ported from Maven 3.9's `DefaultProfileSelector` and the activators in
//! `org.apache.maven.model.profile.activation`. Several upstream behaviours here
//! look like bugs and are reproduced deliberately, because a profile that Maven
//! activates and jv does not means a different dependency tree:
//!
//! - a `<jdk>` range of the form `[1.5]` never activates, because upstream's
//!   parser strips only the leading bracket and then fails to parse `5]`;
//! - `!` on a `<jdk>` value is prefix negation, so `![1.5,)` is negated *prefix*
//!   matching against the literal `[1.5,)` and therefore always true;
//! - an unrecognized `<os><family>` is a plain substring test, not a failure;
//! - a version exactly equal to an inclusive lower bound is in range without the
//!   upper bound being consulted at all;
//! - `!` on a `<property><name>` is ignored whenever a `<value>` is also given.
//!
//! Maven 4's `<packaging>` and `<condition>` activators are recognized only to
//! warn about them: they do not exist in 3.9, so treating them as absent is what
//! keeps jv's answer equal to `mvn`'s.

use std::path::{Path, PathBuf};

use jv_model::{Activation, ActivationFile, ActivationOs, ActivationProperty, Profile, Properties};

use crate::context::{BuildContext, JAVA_VERSION, OS_ARCH, OS_NAME, OS_VERSION};
use crate::problem::Problem;

/// Where a profile was declared, which decides whether the `activeByDefault`
/// suppression rule applies to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileSource {
    /// Declared in a POM. Subject to suppression.
    Pom,
    /// Declared in `settings.xml`. Never suppressed.
    Settings,
}

/// Everything activation is evaluated against.
#[derive(Clone, Copy, Debug)]
pub struct ActivationContext<'a> {
    pub context: &'a BuildContext,
    /// The project directory, for resolving `<file>` paths. Activation on files
    /// cannot work without it.
    pub basedir: Option<&'a Path>,
    /// The properties of the model whose profiles are being activated. Maven 3.9
    /// consults these when interpolating activation values.
    pub model_properties: &'a Properties,
}

/// Chooses which profiles are active, in declaration order.
///
/// Returns indices rather than clones so the caller can inject profiles in the
/// order Maven does without copying them.
pub fn select_active_profiles(
    profiles: &[Profile],
    source: ProfileSource,
    activation: &ActivationContext<'_>,
    problems: &mut Vec<Problem>,
    problem_source: &str,
) -> Vec<usize> {
    let mut active = Vec::new();
    let mut pending_default = Vec::new();
    // Set when any POM profile activates by an explicit request or a condition,
    // which suppresses every activeByDefault profile in the same POM.
    let mut activated_not_by_default = false;

    for (index, profile) in profiles.iter().enumerate() {
        let id = profile.id_or_default();

        // Explicit deactivation beats every other rule.
        if activation
            .context
            .inactive_profiles
            .iter()
            .any(|candidate| candidate == id)
        {
            continue;
        }

        let explicitly_active = activation
            .context
            .active_profiles
            .iter()
            .any(|candidate| candidate == id);
        let by_condition = is_active(profile, activation, problems, problem_source);

        if explicitly_active || by_condition {
            active.push(index);
            if source == ProfileSource::Pom {
                activated_not_by_default = true;
            }
            continue;
        }

        if profile
            .activation
            .as_ref()
            .and_then(|a| a.active_by_default)
            .unwrap_or(false)
        {
            match source {
                // A POM's default profiles wait to see whether anything else won.
                ProfileSource::Pom => pending_default.push(index),
                ProfileSource::Settings => active.push(index),
            }
        }
    }

    if !activated_not_by_default {
        active.extend(pending_default);
    }
    active.sort_unstable();
    active
}

/// Whether a profile's conditions hold.
///
/// All conditions present must pass, and at least one must be present: an empty
/// `<activation/>` activates nothing.
fn is_active(
    profile: &Profile,
    activation: &ActivationContext<'_>,
    problems: &mut Vec<Problem>,
    problem_source: &str,
) -> bool {
    let Some(conditions) = &profile.activation else {
        return false;
    };
    let id = profile.id_or_default();
    let mut any_present = false;

    if let Some(jdk) = &conditions.jdk {
        any_present = true;
        let jdk = interpolate_activation_value(jdk, activation);
        if !jdk_matches(&jdk, activation, problems, problem_source, id) {
            return false;
        }
    }

    if let Some(os) = &conditions.os {
        any_present = true;
        if !os_matches(os, activation) {
            return false;
        }
    }

    if let Some(property) = &conditions.property {
        any_present = true;
        if !property_matches(property, activation, problems, problem_source, id) {
            return false;
        }
    }

    if let Some(file) = &conditions.file {
        any_present = true;
        if !file_matches(file, activation, problems, problem_source, id) {
            return false;
        }
    }

    warn_about_maven_4_activators(conditions, problems, problem_source, id);

    any_present
}

/// Reports Maven 4 activators as unsupported instead of silently mis-evaluating
/// them. They are not counted as present, so a profile relying solely on one
/// stays inactive — which is what Maven 3.9 does, since it cannot even read them.
fn warn_about_maven_4_activators(
    conditions: &Activation,
    problems: &mut Vec<Problem>,
    problem_source: &str,
    id: &str,
) {
    if conditions.packaging.is_some() {
        problems.push(Problem::warning(
            problem_source,
            format!(
                "profile {id} activates on <packaging>, a Maven 4 feature; \
                 jv follows Maven 3.9 and ignores it"
            ),
        ));
    }
    if conditions.condition.is_some() {
        problems.push(Problem::warning(
            problem_source,
            format!(
                "profile {id} activates on <condition>, a Maven 4 feature; \
                 jv follows Maven 3.9 and ignores it"
            ),
        ));
    }
}

/// Expands `${...}` in an activation value.
///
/// Maven 3.9 interpolates activation values (Maven 4 stopped doing so), but only
/// against properties — there is no model to reflect over this early.
fn interpolate_activation_value(text: &str, activation: &ActivationContext<'_>) -> String {
    if !text.contains('$') {
        return text.to_owned();
    }
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
        let resolved = match expression {
            "basedir" | "project.basedir" => activation
                .basedir
                .map(|dir| dir.to_string_lossy().into_owned()),
            // Model properties first, then user, then system — the order
            // `DefaultModelBuilder.interpolateActivations` registers its value
            // sources in, and the reverse of the priority interpolation proper
            // uses. Confirmed against 3.9.9: with `<flavour>frompom</flavour>` in
            // the POM and `-Dflavour=fromcli` on the command line, an activation
            // value of `${flavour}` compares as `frompom`.
            other => activation
                .model_properties
                .get(other)
                .or_else(|| activation.context.user_properties.get(other))
                .or_else(|| activation.context.system_properties.get(other))
                .cloned(),
        };
        match resolved {
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

// ---------------------------------------------------------------- jdk

fn jdk_matches(
    pattern: &str,
    activation: &ActivationContext<'_>,
    problems: &mut Vec<Problem>,
    problem_source: &str,
    id: &str,
) -> bool {
    let Some(current) = activation
        .context
        .property(JAVA_VERSION)
        .filter(|v| !v.is_empty())
    else {
        problems.push(Problem::error(
            problem_source,
            format!(
                "cannot determine the Java version for profile {id}; \
                 <jdk> activation is inactive"
            ),
        ));
        return false;
    };

    // Negation is prefix matching, not range negation.
    if let Some(negated) = pattern.strip_prefix('!') {
        return !current.starts_with(negated);
    }

    // Upstream tests only the opening bracket, so a malformed range still takes
    // the range path.
    if pattern.starts_with('[') || pattern.starts_with('(') {
        return match jdk_version_in_range(current, pattern) {
            Ok(in_range) => in_range,
            Err(reason) => {
                problems.push(Problem::warning(
                    problem_source,
                    format!("profile {id} has an unusable <jdk> range {pattern:?}: {reason}"),
                ));
                false
            }
        };
    }

    current.starts_with(pattern)
}

/// One end of a JDK range.
struct JdkBound {
    value: String,
    inclusive: bool,
}

/// Parses a `<jdk>` range exactly as upstream's `getRange` does, quirks included.
fn parse_jdk_range(pattern: &str) -> Vec<JdkBound> {
    let mut bounds = Vec::new();
    for token in pattern.split(',') {
        // The order of these tests is load-bearing: because the leading-bracket
        // branch wins and strips only that bracket, "[1.5]" yields the bound
        // "1.5]" rather than an exact version.
        let bound = if let Some(rest) = token.strip_prefix('[') {
            Some(JdkBound {
                value: rest.trim().to_owned(),
                inclusive: true,
            })
        } else if let Some(rest) = token.strip_prefix('(') {
            Some(JdkBound {
                value: rest.trim().to_owned(),
                inclusive: false,
            })
        } else if let Some(rest) = token.strip_suffix(']') {
            Some(JdkBound {
                value: rest.trim().to_owned(),
                inclusive: true,
            })
        } else if let Some(rest) = token.strip_suffix(')') {
            Some(JdkBound {
                value: rest.trim().to_owned(),
                inclusive: false,
            })
        } else if token.is_empty() {
            Some(JdkBound {
                value: String::new(),
                inclusive: false,
            })
        } else {
            // A bare token such as "1.5" is silently dropped.
            None
        };
        if let Some(bound) = bound {
            bounds.push(bound);
        }
    }
    if bounds.len() < 2 {
        bounds.push(JdkBound {
            value: "99999999".to_owned(),
            inclusive: false,
        });
    }
    bounds
}

fn jdk_version_in_range(current: &str, pattern: &str) -> Result<bool, String> {
    let bounds = parse_jdk_range(pattern);
    let lower = &bounds[0];
    let upper = &bounds[1];

    let relation = jdk_relation(current, lower, true)?;
    // Equality with the lower bound short-circuits, without consulting the upper
    // bound at all. This is upstream's behaviour, not an optimization.
    if relation == 0 {
        return Ok(true);
    }
    if relation < 0 {
        return Ok(false);
    }
    Ok(jdk_relation(current, upper, false)? <= 0)
}

/// Compares a Java version against one bound, over at most three numeric
/// components.
fn jdk_relation(current: &str, bound: &JdkBound, is_lower: bool) -> Result<i32, String> {
    // An empty bound is unbounded in its direction.
    if bound.value.is_empty() {
        return Ok(if is_lower { 1 } else { -1 });
    }

    // Upstream strips every character that is not a digit or one of . _ -,
    // which is why "1.8.0_292" compares as 1.8.0 and build metadata folds into
    // the preceding component.
    let filtered: String = current
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        .collect();
    let current_parts: Vec<&str> = filtered.split(['.', '_', '-']).collect();
    let bound_parts: Vec<&str> = bound.value.split('.').collect();

    for index in 0..3 {
        let current_component = parse_component(current_parts.get(index).copied())
            .map_err(|token| format!("{token:?} in the Java version is not a number"))?;
        let bound_component = parse_component(bound_parts.get(index).copied())
            .map_err(|token| format!("{token:?} in the bound is not a number"))?;
        if current_component != bound_component {
            return Ok(if current_component > bound_component {
                1
            } else {
                -1
            });
        }
    }

    // Every compared component is equal, so inclusivity decides.
    Ok(if bound.inclusive {
        0
    } else if is_lower {
        -1
    } else {
        1
    })
}

/// Parses one version component, treating an absent component as zero padding.
fn parse_component(token: Option<&str>) -> Result<i64, String> {
    match token {
        None => Ok(0),
        Some(text) => text.parse::<i64>().map_err(|_| text.to_owned()),
    }
}

// ---------------------------------------------------------------- os

fn os_matches(os: &ActivationOs, activation: &ActivationContext<'_>) -> bool {
    // With nothing declared there is nothing to match.
    if os.name.is_none() && os.family.is_none() && os.arch.is_none() && os.version.is_none() {
        return false;
    }

    let actual_name = os_property(activation, OS_NAME);
    let actual_arch = os_property(activation, OS_ARCH);
    let actual_version = os_property(activation, OS_VERSION);
    let path_separator = activation
        .context
        .property("path.separator")
        .map(str::to_owned)
        .unwrap_or_else(|| if cfg!(windows) { ";" } else { ":" }.to_owned());

    // Evaluated family, name, arch, version — all ANDed.
    if let Some(family) = &os.family {
        // Lower-cased first: Maven 3.9's `determineFamilyMatch` does
        // `family.toLowerCase(Locale.ENGLISH)` before comparing, so
        // `<family>Unix</family>` matches on Linux. (Maven 4 dropped that, which
        // is what the code here used to implement.) Verified against 3.9.9.
        let family = interpolate_activation_value(family, activation).to_lowercase();
        let (negated, family) = split_negation(&family);
        if negated == is_family(family, &actual_name, &path_separator) {
            return false;
        }
    }
    if let Some(name) = &os.name {
        let name = interpolate_activation_value(name, activation).to_lowercase();
        let (negated, name) = split_negation(&name);
        if negated == (actual_name == name) {
            return false;
        }
    }
    if let Some(arch) = &os.arch {
        let arch = interpolate_activation_value(arch, activation).to_lowercase();
        let (negated, arch) = split_negation(&arch);
        if negated == (actual_arch == arch) {
            return false;
        }
    }
    if let Some(version) = &os.version {
        let version = interpolate_activation_value(version, activation);
        // `regex:` is checked before negation, so `!regex:…` is a literal
        // comparison against the whole string.
        //
        // The pattern keeps its case. Only the plain-comparison branch is
        // lower-cased, because upstream lower-cases the *value* it compares
        // against and uses the pattern as written — `regex:.*MICROSOFT.*`
        // matches nothing on a host whose `os.version` is lower-case, and jv
        // used to match it by folding the pattern too.
        if let Some(pattern) = version.strip_prefix("regex:") {
            if !regex_full_match(pattern, &actual_version) {
                return false;
            }
        } else {
            let version = version.to_lowercase();
            let (negated, version) = split_negation(&version);
            if negated == (actual_version == version) {
                return false;
            }
        }
    }
    true
}

fn os_property(activation: &ActivationContext<'_>, key: &str) -> String {
    activation
        .context
        .property(key)
        .unwrap_or_default()
        .to_lowercase()
}

/// Splits a leading `!`, which negates that one comparison.
fn split_negation(value: &str) -> (bool, &str) {
    match value.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, value),
    }
}

/// Upstream's `Os.isFamily`, including its substring fallback for unrecognized
/// families.
fn is_family(family: &str, os_name: &str, path_separator: &str) -> bool {
    match family {
        "windows" => os_name.contains("windows"),
        "win9x" => {
            os_name.contains("windows")
                && (os_name.contains("95")
                    || os_name.contains("98")
                    || os_name.contains("me")
                    // Bare "ce", not "windows ce".
                    || os_name.contains("ce"))
        }
        "winnt" => os_name.contains("windows") && !is_family("win9x", os_name, path_separator),
        "dos" => {
            path_separator == ";"
                && !is_family("netware", os_name, path_separator)
                && !is_family("windows", os_name, path_separator)
        }
        "mac" => os_name.contains("mac") || os_name.contains("darwin"),
        "unix" => {
            path_separator == ":"
                && !is_family("openvms", os_name, path_separator)
                // "mac os x" counts as unix; classic Mac OS does not.
                && (!is_family("mac", os_name, path_separator) || os_name.ends_with('x'))
        }
        "os/2" => os_name.contains("os/2"),
        "netware" => os_name.contains("netware"),
        "tandem" => os_name.contains("nonstop_kernel"),
        "openvms" => os_name.contains("openvms"),
        "z/os" | "os/390" => os_name.contains("z/os") || os_name.contains("os/390"),
        "os/400" => os_name.contains("os/400"),
        // Anything else is a substring test rather than a failure.
        other => os_name.contains(&other.to_lowercase()),
    }
}

/// A whole-string match for the small subset of regular expressions that appear
/// in `<os><version>` activators.
///
/// Bringing in a regex engine for this would add a large dependency to a code
/// path a handful of POMs use. Supported: literals, `.`, `*`, `+`, `?`,
/// character classes, and alternation at the top level. Anything else is
/// reported as unmatched rather than silently mis-evaluated.
fn regex_full_match(pattern: &str, text: &str) -> bool {
    // Alternation splits the pattern at top level.
    if let Some(alternatives) = split_top_level_alternation(pattern) {
        return alternatives
            .iter()
            .any(|alternative| regex_full_match(alternative, text));
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    regex_match_from(&pattern, 0, &text, 0)
}

fn split_top_level_alternation(pattern: &str) -> Option<Vec<&str>> {
    let mut depth = 0usize;
    let mut in_class = false;
    let mut parts = Vec::new();
    let mut start = 0usize;
    for (index, ch) in pattern.char_indices() {
        match ch {
            '[' => in_class = true,
            ']' => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth = depth.saturating_sub(1),
            '|' if !in_class && depth == 0 => {
                parts.push(&pattern[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(&pattern[start..]);
    Some(parts)
}

/// Backtracking matcher over the supported subset.
fn regex_match_from(pattern: &[char], pi: usize, text: &[char], ti: usize) -> bool {
    if pi >= pattern.len() {
        return ti >= text.len();
    }
    // Parse one atom and any quantifier that follows it.
    let (atom_end, matcher) = match parse_atom(pattern, pi) {
        Some(parsed) => parsed,
        // An unsupported construct: report no match rather than guess.
        None => return false,
    };
    let quantifier = pattern.get(atom_end).copied();
    match quantifier {
        Some('*') | Some('+') => {
            let minimum = usize::from(quantifier == Some('+'));
            let next = atom_end + 1;
            let mut count = 0usize;
            // Greedy, with backtracking.
            while ti + count < text.len() && matcher(text[ti + count]) {
                count += 1;
            }
            loop {
                if count >= minimum && regex_match_from(pattern, next, text, ti + count) {
                    return true;
                }
                if count == 0 {
                    return false;
                }
                count -= 1;
            }
        }
        Some('?') => {
            let next = atom_end + 1;
            if ti < text.len() && matcher(text[ti]) && regex_match_from(pattern, next, text, ti + 1)
            {
                return true;
            }
            regex_match_from(pattern, next, text, ti)
        }
        _ => {
            if ti < text.len() && matcher(text[ti]) {
                regex_match_from(pattern, atom_end, text, ti + 1)
            } else {
                false
            }
        }
    }
}

/// Parses a single-character-matching atom, returning where it ends.
type CharMatcher = Box<dyn Fn(char) -> bool>;

fn parse_atom(pattern: &[char], pi: usize) -> Option<(usize, CharMatcher)> {
    match pattern[pi] {
        '.' => Some((pi + 1, Box::new(|_| true))),
        '\\' => {
            let escaped = *pattern.get(pi + 1)?;
            let matcher: CharMatcher = match escaped {
                'd' => Box::new(|c: char| c.is_ascii_digit()),
                'w' => Box::new(|c: char| c.is_alphanumeric() || c == '_'),
                's' => Box::new(char::is_whitespace),
                literal => Box::new(move |c: char| c == literal),
            };
            Some((pi + 2, matcher))
        }
        '[' => {
            let close = pattern[pi..].iter().position(|c| *c == ']')? + pi;
            let mut body = &pattern[pi + 1..close];
            let negated = body.first() == Some(&'^');
            if negated {
                body = &body[1..];
            }
            let set: Vec<char> = body.to_vec();
            Some((
                close + 1,
                Box::new(move |c: char| {
                    let mut hit = false;
                    let mut index = 0usize;
                    while index < set.len() {
                        // A range such as a-z.
                        if index + 2 < set.len() && set[index + 1] == '-' {
                            if c >= set[index] && c <= set[index + 2] {
                                hit = true;
                            }
                            index += 3;
                        } else {
                            if c == set[index] {
                                hit = true;
                            }
                            index += 1;
                        }
                    }
                    hit != negated
                }),
            ))
        }
        // Groups and anchors are not supported; the caller treats that as no
        // match rather than pretending.
        '(' | ')' | '^' | '$' | '{' | '}' => None,
        literal => Some((pi + 1, Box::new(move |c: char| c == literal))),
    }
}

// ---------------------------------------------------------------- property

fn property_matches(
    property: &ActivationProperty,
    activation: &ActivationContext<'_>,
    problems: &mut Vec<Problem>,
    problem_source: &str,
    id: &str,
) -> bool {
    let raw_name = property.name.as_deref().unwrap_or("");
    let name = interpolate_activation_value(raw_name, activation);
    let (reverse_name, name) = split_negation(&name);

    if name.is_empty() {
        problems.push(Problem::error(
            problem_source,
            format!("the property name is required to activate profile {id}"),
        ));
        return false;
    }

    let actual = activation.context.property(name);

    match property.value.as_deref().filter(|value| !value.is_empty()) {
        Some(declared) => {
            // With a value present, the `!` on the name is silently ignored —
            // only the value's own negation counts.
            let declared = interpolate_activation_value(declared, activation);
            let (reverse_value, declared) = split_negation(&declared);
            // Comparison is against the declared string, so an unset property
            // never equals it: `<value>!x</value>` holds when unset.
            reverse_value != (actual == Some(declared))
        }
        // Existence test: an empty value counts as absent.
        None => reverse_name != actual.is_some_and(|value| !value.is_empty()),
    }
}

// ---------------------------------------------------------------- file

fn file_matches(
    file: &ActivationFile,
    activation: &ActivationContext<'_>,
    problems: &mut Vec<Problem>,
    problem_source: &str,
    id: &str,
) -> bool {
    let exists = file.exists.as_deref().filter(|path| !path.is_empty());
    let missing = file.missing.as_deref().filter(|path| !path.is_empty());

    let (path, expect_missing) = match (exists, missing) {
        (None, None) => return false,
        (Some(path), Some(_)) => {
            problems.push(Problem::warning(
                problem_source,
                format!(
                    "profile {id} declares both <exists> and <missing>; \
                     the <missing> assertion is ignored"
                ),
            ));
            (path, false)
        }
        (Some(path), None) => (path, false),
        (None, Some(path)) => (path, true),
    };

    let interpolated = interpolate_activation_value(path, activation);
    // A relative path is resolved against the project directory, so file
    // activation is meaningless without one.
    let resolved = match activation.basedir {
        Some(basedir) => basedir.join(&interpolated),
        None => PathBuf::from(&interpolated),
    };
    // No glob support: `*` is a literal character here.
    let present = resolved.exists();
    expect_missing != present
}

#[cfg(test)]
mod tests {
    use super::*;

    use jv_model::parse_pom;

    fn profiles_of(xml: &str) -> Vec<Profile> {
        parse_pom(xml).expect("parse").model.profiles
    }

    fn activate(
        profiles: &[Profile],
        context: &BuildContext,
        basedir: Option<&Path>,
    ) -> (Vec<String>, Vec<Problem>) {
        let properties = Properties::new();
        let activation = ActivationContext {
            context,
            basedir,
            model_properties: &properties,
        };
        let mut problems = Vec::new();
        let active = select_active_profiles(
            profiles,
            ProfileSource::Pom,
            &activation,
            &mut problems,
            "test",
        );
        (
            active
                .into_iter()
                .map(|index| profiles[index].id_or_default().to_owned())
                .collect(),
            problems,
        )
    }

    #[test]
    fn empty_activation_activates_nothing() {
        let profiles = profiles_of(
            r#"<project><profiles>
                 <profile><id>p</id><activation/></profile>
                 <profile><id>q</id></profile>
               </profiles></project>"#,
        );
        let (active, _) = activate(&profiles, &BuildContext::empty(), None);
        assert!(active.is_empty());
    }

    #[test]
    fn explicit_activation_and_deactivation() {
        let profiles = profiles_of(
            r#"<project><profiles>
                 <profile><id>p</id></profile>
                 <profile><id>q</id><activation><activeByDefault>true</activeByDefault></activation></profile>
               </profiles></project>"#,
        );
        let mut context = BuildContext::empty();
        context.active_profiles.push("p".to_owned());
        let (active, _) = activate(&profiles, &context, None);
        // Activating p suppresses q's activeByDefault.
        assert_eq!(active, vec!["p"]);

        // Deactivation wins even over an explicit request.
        let mut both = BuildContext::empty();
        both.active_profiles.push("p".to_owned());
        both.inactive_profiles.push("p".to_owned());
        let (active, _) = activate(&profiles, &both, None);
        assert_eq!(active, vec!["q"]);
    }

    #[test]
    fn active_by_default_is_suppressed_by_any_other_activation() {
        let profiles = profiles_of(
            r#"<project><profiles>
                 <profile><id>fallback</id>
                   <activation><activeByDefault>true</activeByDefault></activation></profile>
                 <profile><id>conditional</id>
                   <activation><property><name>ci</name></property></activation></profile>
               </profiles></project>"#,
        );
        let (active, _) = activate(&profiles, &BuildContext::empty(), None);
        assert_eq!(active, vec!["fallback"]);

        let context = BuildContext::empty().with_system_property("ci", "yes");
        let (active, _) = activate(&profiles, &context, None);
        assert_eq!(active, vec!["conditional"]);
    }

    #[test]
    fn settings_defaults_are_never_suppressed() {
        let profiles = profiles_of(
            r#"<project><profiles>
                 <profile><id>d</id>
                   <activation><activeByDefault>true</activeByDefault></activation></profile>
                 <profile><id>c</id>
                   <activation><property><name>ci</name></property></activation></profile>
               </profiles></project>"#,
        );
        let context = BuildContext::empty().with_system_property("ci", "yes");
        let properties = Properties::new();
        let activation = ActivationContext {
            context: &context,
            basedir: None,
            model_properties: &properties,
        };
        let mut problems = Vec::new();
        let active = select_active_profiles(
            &profiles,
            ProfileSource::Settings,
            &activation,
            &mut problems,
            "settings.xml",
        );
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn multiple_conditions_are_anded() {
        let profiles = profiles_of(
            r#"<project><profiles><profile>
                 <id>p</id>
                 <activation>
                   <property><name>ci</name></property>
                   <jdk>21</jdk>
                 </activation>
               </profile></profiles></project>"#,
        );
        // Only one holds.
        let context = BuildContext::empty().with_system_property("ci", "yes");
        assert!(activate(&profiles, &context, None).0.is_empty());
        // Both hold.
        let context = context.with_java_version("21.0.11");
        assert_eq!(activate(&profiles, &context, None).0, vec!["p"]);
    }

    fn jdk_profile(pattern: &str) -> Vec<Profile> {
        profiles_of(&format!(
            r#"<project><profiles><profile><id>p</id>
                 <activation><jdk>{pattern}</jdk></activation>
               </profile></profiles></project>"#
        ))
    }

    fn jdk_active(pattern: &str, java_version: &str) -> bool {
        let profiles = jdk_profile(pattern);
        let context = BuildContext::empty().with_java_version(java_version);
        !activate(&profiles, &context, None).0.is_empty()
    }

    #[test]
    fn jdk_prefix_matching() {
        assert!(jdk_active("1.4", "1.4.2_08"));
        assert!(!jdk_active("1.4", "1.5.0"));
        assert!(jdk_active("21", "21.0.11"));
        // Prefix matching on the whole string, so "11" does not match "1.1".
        assert!(!jdk_active("11", "1.1.0"));
        // And a bare "1" matches both 1.8 and 17, a documented gotcha.
        assert!(jdk_active("1", "1.8.0"));
        assert!(jdk_active("1", "17"));
    }

    #[test]
    fn jdk_negation_is_prefix_negation() {
        assert!(!jdk_active("!1.4", "1.4.2"));
        assert!(jdk_active("!1.4", "1.5.0"));
        // Negating a range negates the literal text, so it is always true.
        assert!(jdk_active("![1.5,)", "1.4.0"));
        assert!(jdk_active("![1.5,)", "21.0.1"));
    }

    #[test]
    fn jdk_ranges() {
        assert!(jdk_active("[1.5,)", "1.8.0"));
        assert!(jdk_active("[1.5,)", "21"));
        assert!(!jdk_active("[1.5,)", "1.4.2"));
        assert!(jdk_active("(,1.8]", "1.7.0"));
        assert!(jdk_active("(,1.8]", "1.8.0"));
        assert!(!jdk_active("(,1.8]", "1.9.0"));
        assert!(jdk_active("[11,17)", "11.0.2"));
        assert!(!jdk_active("[11,17)", "17.0.1"));
        assert!(jdk_active("[11,17)", "16.0.9"));
    }

    #[test]
    fn jdk_exact_range_form_never_activates() {
        // "[1.5]" parses as the bound "1.5]", which fails to parse as a number.
        let profiles = jdk_profile("[1.5]");
        let context = BuildContext::empty().with_java_version("1.5.0");
        let (active, problems) = activate(&profiles, &context, None);
        assert!(active.is_empty());
        assert!(problems.iter().any(|p| p.message.contains("unusable")));
    }

    #[test]
    fn jdk_three_component_truncation() {
        // Update levels beyond the third component are invisible.
        assert_eq!(
            jdk_active("[1.8.0,)", "1.8.0_292"),
            jdk_active("[1.8.0,)", "1.8.0_100")
        );
        assert!(jdk_active("[1.8.0,)", "1.8.0_292"));
    }

    #[test]
    fn jdk_equal_to_inclusive_lower_bound_skips_the_upper_bound() {
        // Upstream returns true as soon as the lower bound compares equal, even
        // when the upper bound would exclude the version.
        assert!(jdk_active("[11,11)", "11"));
    }

    #[test]
    fn jdk_without_a_java_version_reports_an_error() {
        let profiles = jdk_profile("21");
        let (active, problems) = activate(&profiles, &BuildContext::empty(), None);
        assert!(active.is_empty());
        assert!(problems.iter().any(|p| p.message.contains("Java version")));
    }

    fn os_active(activation: &str, name: &str, arch: &str, version: &str) -> bool {
        let profiles = profiles_of(&format!(
            r#"<project><profiles><profile><id>p</id>
                 <activation><os>{activation}</os></activation>
               </profile></profiles></project>"#
        ));
        let context = BuildContext::empty()
            .with_system_property(OS_NAME, name)
            .with_system_property(OS_ARCH, arch)
            .with_system_property(OS_VERSION, version)
            .with_system_property("path.separator", ":");
        !activate(&profiles, &context, None).0.is_empty()
    }

    /// Both checked against a real Maven 3.9.9 before being written down.
    #[test]
    fn an_os_family_is_matched_case_insensitively() {
        // Maven 3.9's `determineFamilyMatch` lower-cases the declared family
        // first. Maven 4 dropped that, and jv had implemented Maven 4's rule, so
        // `<family>Unix</family>` silently stopped matching on Linux — and an
        // activator that never matches looks exactly like one whose condition is
        // false.
        for spelling in ["unix", "Unix", "UNIX"] {
            assert!(
                os_active(
                    &format!("<family>{spelling}</family>"),
                    "Linux",
                    "amd64",
                    "6.1"
                ),
                "{spelling} should match"
            );
        }
        assert!(!os_active(
            "<family>Windows</family>",
            "Linux",
            "amd64",
            "6.1"
        ));
    }

    #[test]
    fn an_os_version_regex_keeps_its_case_while_a_plain_comparison_does_not() {
        // Upstream lower-cases the value it compares against and uses the
        // pattern as written, so a pattern spelled in upper case matches
        // nothing. Folding the pattern too made it match, which Maven does not.
        assert!(os_active(
            "<version>regex:.*microsoft.*</version>",
            "Linux",
            "amd64",
            "6.6.87-microsoft-standard"
        ));
        assert!(!os_active(
            "<version>regex:.*MICROSOFT.*</version>",
            "Linux",
            "amd64",
            "6.6.87-microsoft-standard"
        ));
        // A plain comparison stays case-insensitive.
        assert!(os_active("<version>6.1</version>", "Linux", "amd64", "6.1"));
        assert!(os_active(
            "<version>6.1-RC</version>",
            "Linux",
            "amd64",
            "6.1-rc"
        ));
    }

    #[test]
    fn os_family_and_name() {
        assert!(os_active("<family>unix</family>", "Linux", "amd64", "6.1"));
        assert!(!os_active(
            "<family>windows</family>",
            "Linux",
            "amd64",
            "6.1"
        ));
        assert!(os_active("<name>linux</name>", "Linux", "amd64", "6.1"));
        // Comparison is case-insensitive because both sides are lower-cased.
        assert!(os_active("<name>LINUX</name>", "Linux", "amd64", "6.1"));
        // mac os x counts as unix.
        assert!(os_active(
            "<family>unix</family>",
            "Mac OS X",
            "aarch64",
            "23.0"
        ));
        assert!(os_active(
            "<family>mac</family>",
            "Mac OS X",
            "aarch64",
            "23.0"
        ));
    }

    #[test]
    fn os_unknown_family_falls_back_to_substring() {
        // Not a recognized family, so it is a substring test rather than false.
        assert!(os_active("<family>linux</family>", "Linux", "amd64", "6.1"));
        assert!(!os_active(
            "<family>solaris</family>",
            "Linux",
            "amd64",
            "6.1"
        ));
        // Mixed case still works through the fallback.
        assert!(os_active("<family>Linux</family>", "Linux", "amd64", "6.1"));
    }

    #[test]
    fn os_negation_and_arch() {
        assert!(os_active("<arch>amd64</arch>", "Linux", "amd64", "6.1"));
        assert!(!os_active("<arch>aarch64</arch>", "Linux", "amd64", "6.1"));
        assert!(os_active("<arch>!aarch64</arch>", "Linux", "amd64", "6.1"));
        assert!(!os_active(
            "<family>!unix</family>",
            "Linux",
            "amd64",
            "6.1"
        ));
    }

    #[test]
    fn os_fields_are_anded() {
        assert!(os_active(
            "<family>unix</family><arch>amd64</arch>",
            "Linux",
            "amd64",
            "6.1"
        ));
        assert!(!os_active(
            "<family>unix</family><arch>aarch64</arch>",
            "Linux",
            "amd64",
            "6.1"
        ));
    }

    #[test]
    fn os_version_regex() {
        assert!(os_active(
            "<version>regex:6\\.1.*</version>",
            "Linux",
            "amd64",
            "6.1.87"
        ));
        assert!(!os_active(
            "<version>regex:5\\..*</version>",
            "Linux",
            "amd64",
            "6.1.87"
        ));
        // A full-string match, not a search.
        assert!(!os_active(
            "<version>regex:6\\.1</version>",
            "Linux",
            "amd64",
            "6.1.87"
        ));
    }

    #[test]
    fn os_with_nothing_declared_is_inactive() {
        assert!(!os_active("", "Linux", "amd64", "6.1"));
    }

    fn property_active(activation: &str, context: &BuildContext) -> bool {
        let profiles = profiles_of(&format!(
            r#"<project><profiles><profile><id>p</id>
                 <activation><property>{activation}</property></activation>
               </profile></profiles></project>"#
        ));
        !activate(&profiles, context, None).0.is_empty()
    }

    #[test]
    fn property_existence_tests() {
        let unset = BuildContext::empty();
        let set = BuildContext::empty().with_system_property("debug", "1");
        let empty = BuildContext::empty().with_system_property("debug", "");

        assert!(!property_active("<name>debug</name>", &unset));
        assert!(property_active("<name>debug</name>", &set));
        // An empty value counts as absent.
        assert!(!property_active("<name>debug</name>", &empty));

        assert!(property_active("<name>!debug</name>", &unset));
        assert!(!property_active("<name>!debug</name>", &set));
        assert!(property_active("<name>!debug</name>", &empty));
    }

    #[test]
    fn property_value_tests() {
        let prod = BuildContext::empty().with_system_property("env", "prod");
        let dev = BuildContext::empty().with_system_property("env", "dev");
        let unset = BuildContext::empty();

        assert!(property_active(
            "<name>env</name><value>prod</value>",
            &prod
        ));
        assert!(!property_active(
            "<name>env</name><value>prod</value>",
            &dev
        ));
        // An unset property never equals a declared value.
        assert!(!property_active(
            "<name>env</name><value>prod</value>",
            &unset
        ));

        assert!(!property_active(
            "<name>env</name><value>!prod</value>",
            &prod
        ));
        assert!(property_active(
            "<name>env</name><value>!prod</value>",
            &dev
        ));
        // Negated value matching holds when the property is unset.
        assert!(property_active(
            "<name>env</name><value>!prod</value>",
            &unset
        ));
    }

    #[test]
    fn negated_name_is_ignored_when_a_value_is_present() {
        let prod = BuildContext::empty().with_system_property("env", "prod");
        // The `!` on the name is dropped; this is a plain value comparison.
        assert!(property_active(
            "<name>!env</name><value>prod</value>",
            &prod
        ));
    }

    #[test]
    fn property_reads_the_command_line_first() {
        let context = BuildContext::empty()
            .with_system_property("env", "dev")
            .with_user_property("env", "prod");
        assert!(property_active(
            "<name>env</name><value>prod</value>",
            &context
        ));
    }

    #[test]
    fn missing_property_name_is_an_error() {
        let profiles = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><property><value>x</value></property></activation>
               </profile></profiles></project>"#,
        );
        let (active, problems) = activate(&profiles, &BuildContext::empty(), None);
        assert!(active.is_empty());
        assert!(problems.iter().any(|p| p.message.contains("required")));
    }

    #[test]
    fn file_activation() {
        let dir = std::env::temp_dir().join("jv-activation-test");
        let _ = std::fs::create_dir_all(&dir);
        let marker = dir.join("marker.txt");
        let _ = std::fs::write(&marker, b"x");

        let exists = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><file><exists>marker.txt</exists></file></activation>
               </profile></profiles></project>"#,
        );
        assert_eq!(
            activate(&exists, &BuildContext::empty(), Some(&dir)).0,
            vec!["p"]
        );

        let missing = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><file><missing>absent.txt</missing></file></activation>
               </profile></profiles></project>"#,
        );
        assert_eq!(
            activate(&missing, &BuildContext::empty(), Some(&dir)).0,
            vec!["p"]
        );

        let missing_but_present = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><file><missing>marker.txt</missing></file></activation>
               </profile></profiles></project>"#,
        );
        assert!(
            activate(&missing_but_present, &BuildContext::empty(), Some(&dir))
                .0
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_activation_prefers_exists_and_warns() {
        let dir = std::env::temp_dir().join("jv-activation-conflict");
        let _ = std::fs::create_dir_all(&dir);
        let profiles = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><file>
                   <exists>nope.txt</exists><missing>nope.txt</missing>
                 </file></activation>
               </profile></profiles></project>"#,
        );
        let (active, problems) = activate(&profiles, &BuildContext::empty(), Some(&dir));
        // <exists> wins, and the file is absent, so the profile is inactive.
        assert!(active.is_empty());
        assert!(problems.iter().any(|p| p.message.contains("ignored")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_activation_interpolates_basedir() {
        let dir = std::env::temp_dir().join("jv-activation-basedir");
        let _ = std::fs::create_dir_all(&dir);
        let marker = dir.join("flag");
        let _ = std::fs::write(&marker, b"x");
        let profiles = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><file><exists>${basedir}/flag</exists></file></activation>
               </profile></profiles></project>"#,
        );
        assert_eq!(
            activate(&profiles, &BuildContext::empty(), Some(&dir)).0,
            vec!["p"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maven_4_activators_warn_and_do_not_activate() {
        let profiles = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><packaging>jar</packaging></activation>
               </profile></profiles></project>"#,
        );
        let (active, problems) = activate(&profiles, &BuildContext::empty(), None);
        assert!(active.is_empty());
        assert!(problems.iter().any(|p| p.message.contains("Maven 4")));
    }

    #[test]
    fn activation_values_are_interpolated() {
        // Maven 3.9 interpolates activation values; Maven 4 does not.
        let profiles = profiles_of(
            r#"<project><profiles><profile><id>p</id>
                 <activation><property>
                   <name>target</name><value>${wanted}</value>
                 </property></activation>
               </profile></profiles></project>"#,
        );
        let mut properties = Properties::new();
        properties.insert("wanted".to_owned(), "prod".to_owned());
        let context = BuildContext::empty().with_system_property("target", "prod");
        let activation = ActivationContext {
            context: &context,
            basedir: None,
            model_properties: &properties,
        };
        let mut problems = Vec::new();
        let active = select_active_profiles(
            &profiles,
            ProfileSource::Pom,
            &activation,
            &mut problems,
            "test",
        );
        assert_eq!(active, vec![0]);
    }

    #[test]
    fn regex_subset_behaves() {
        assert!(regex_full_match("abc", "abc"));
        assert!(!regex_full_match("abc", "abcd"));
        assert!(regex_full_match("a.c", "abc"));
        assert!(regex_full_match("ab*c", "ac"));
        assert!(regex_full_match("ab+c", "abbc"));
        assert!(!regex_full_match("ab+c", "ac"));
        assert!(regex_full_match("ab?c", "ac"));
        assert!(regex_full_match("[0-9]+\\.[0-9]+", "6.1"));
        assert!(!regex_full_match("[0-9]+\\.[0-9]+", "6.1.2"));
        assert!(regex_full_match("6\\..*", "6.1.87"));
        assert!(regex_full_match("5\\..*|6\\..*", "6.1"));
        assert!(!regex_full_match("5\\..*|7\\..*", "6.1"));
        assert!(regex_full_match("[^a]bc", "xbc"));
        assert!(!regex_full_match("[^a]bc", "abc"));
        // Unsupported constructs match nothing rather than guessing.
        assert!(!regex_full_match("(a)bc", "abc"));
    }
}
