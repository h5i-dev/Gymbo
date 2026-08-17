//! What `jvx <endpoint>` names.
//!
//! # Grammar
//!
//! ```text
//! endpoint    := coordinate [ '@' main-class ]
//! coordinate  := group ':' artifact [ ':' version [ ':' classifier ] ]
//! ```
//!
//! So all of these are endpoints:
//!
//! ```text
//! com.google.googlejavaformat:google-java-format
//! org.openrewrite:rewrite-maven-plugin:5.0.0
//! org.lwjgl:lwjgl:3.3.4:natives-linux
//! org.scijava:scijava-common:2.99.0@org.scijava.script.ScriptREPL
//! ```
//!
//! # The one ambiguity, and the rule that settles it
//!
//! `g:a:v:x` is ambiguous: `x` could be a classifier or, in jgo's older
//! spelling, a main class. So is `g:a:x`, where `x` could be a version or a main
//! class. Nothing about the position tells them apart, so the rule is about the
//! *shape* of the last field:
//!
//! > In a three- or four-field coordinate with no `@`, the last field is read as
//! > a main class only when it is a dotted, capitalised Java class name — every
//! > `.`-separated token is a Java identifier, and the final token starts with an
//! > uppercase letter. Otherwise the fields are strictly positional.
//!
//! `com.example.Main` qualifies. `1.36.1` does not (a token may not start with a
//! digit), nor does `4.1.100.Final`, nor `RELEASE`, nor `natives-linux` (`-` is
//! not an identifier character), nor `sources` (no dot). That is the whole test.
//!
//! It is deliberately stricter than jgo's, which accepts a dotless capitalised
//! token too. The two failure modes are not symmetric: reading a classifier as a
//! main class produces a resolve of the wrong artifact and a confusing error
//! several seconds later, while refusing to read a main class produces an
//! immediate error that names the `@` spelling. So the heuristic only fires
//! where it is nearly certain, and `@` is always available as the exact one.
//!
//! [`Endpoint::parse_positional`] switches the heuristic off entirely. `jvx`
//! uses it when `--main` was given, because then the last field cannot be a main
//! class — which makes `--main` the escape hatch for the one coordinate the
//! heuristic would misread, a classifier that looks like a class name.
//!
//! # Dropped from jgo
//!
//! - `+`-joined coordinates (`g:a+g2:a2@Main`). Doing it properly means one
//!   collection over several roots so that conflict resolution sees the whole
//!   picture; concatenating independent resolves would build a classpath Maven
//!   would never build. Deferred rather than approximated.
//! - The `!` raw/unmanaged suffix and the `(c)`/`(m)` module-path suffixes; the
//!   latter belong with JPMS launching, which `ROADMAP.md` puts at v0.3.
//! - jgo's `G:A:P:C:V:S` orderings and its "strict mode" empty fields. jvx reads
//!   one order, the one people type.

use jv_model::Artifact;

/// A parsed endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub group_id: String,
    pub artifact_id: String,
    /// `None` asks for the greatest release; see [`crate::release`].
    pub version: Option<String>,
    pub classifier: Option<String>,
    /// The `@`-named or heuristically-recognised main class, if the endpoint
    /// named one.
    pub main_class: Option<String>,
}

/// An endpoint that cannot be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error("an endpoint is required; expected group:artifact[:version[:classifier]][@mainClass]")]
    Empty,
    #[error("`{0}` names no artifact; expected group:artifact[:version[:classifier]][@mainClass]")]
    TooFewFields(String),
    #[error(
        "`{0}` has too many `:`-separated fields; expected at most group:artifact:version:classifier"
    )]
    TooManyFields(String),
    #[error("`{0}` names a main class twice; only one `@` is allowed")]
    MultipleMainClasses(String),
    #[error("`{0}` has an empty field; every part of a coordinate must be spelled out")]
    EmptyField(String),
    #[error("`{0}` ends in `@` without naming a class")]
    EmptyMainClass(String),
}

/// Whether the last field of a bare coordinate may be a main class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trailing {
    MayNameAClass,
    Positional,
}

impl Endpoint {
    /// Parses an endpoint, applying the main-class heuristic documented above.
    pub fn parse(text: &str) -> Result<Self, EndpointError> {
        parse(text, Trailing::MayNameAClass)
    }

    /// Parses an endpoint whose fields are strictly positional.
    ///
    /// For callers that already know the main class, and for whom the last field
    /// therefore cannot be one.
    pub fn parse_positional(text: &str) -> Result<Self, EndpointError> {
        parse(text, Trailing::Positional)
    }

    /// The coordinate half, as written back out. Used in error messages, where
    /// echoing the main class as well would only add noise.
    pub fn coordinates(&self) -> String {
        let mut text = format!("{}:{}", self.group_id, self.artifact_id);
        if let Some(version) = &self.version {
            text.push(':');
            text.push_str(version);
        }
        if let Some(classifier) = &self.classifier {
            // A classifier with no version is legal in an endpoint but not in a
            // coordinate, so the placeholder keeps the field positions readable.
            if self.version.is_none() {
                text.push_str(":*");
            }
            text.push(':');
            text.push_str(classifier);
        }
        text
    }

    /// The artifact this endpoint names, once a version is known.
    pub fn artifact(&self, version: &str) -> Artifact {
        let artifact = Artifact::new(&self.group_id, &self.artifact_id, version);
        match &self.classifier {
            Some(classifier) => artifact.with_classifier(classifier),
            None => artifact,
        }
    }
}

fn parse(text: &str, trailing: Trailing) -> Result<Endpoint, EndpointError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(EndpointError::Empty);
    }

    let mut main_class = None;
    let coordinate = match text.split('@').count() {
        1 => text,
        2 => {
            let (coordinate, named) = text.split_once('@').expect("one `@` was counted");
            if named.is_empty() {
                return Err(EndpointError::EmptyMainClass(text.to_owned()));
            }
            main_class = Some(named.to_owned());
            coordinate
        }
        // jgo raises here too rather than picking one, and it is right to: an
        // endpoint with two main classes has no reading its author intended.
        _ => return Err(EndpointError::MultipleMainClasses(text.to_owned())),
    };

    let mut fields: Vec<&str> = coordinate.split(':').collect();
    if main_class.is_none()
        && trailing == Trailing::MayNameAClass
        && fields.len() >= 3
        && names_a_class(fields[fields.len() - 1])
    {
        main_class = fields.pop().map(str::to_owned);
    }

    if fields.len() < 2 {
        return Err(EndpointError::TooFewFields(text.to_owned()));
    }
    if fields.len() > 4 {
        return Err(EndpointError::TooManyFields(text.to_owned()));
    }
    if fields.iter().any(|field| field.is_empty()) {
        return Err(EndpointError::EmptyField(text.to_owned()));
    }

    Ok(Endpoint {
        group_id: fields[0].to_owned(),
        artifact_id: fields[1].to_owned(),
        version: fields.get(2).map(|field| (*field).to_owned()),
        classifier: fields.get(3).map(|field| (*field).to_owned()),
        main_class,
    })
}

/// Whether a field is unmistakably a fully qualified Java class name.
///
/// "Unmistakably" is the whole point; see the module docs for why the bar is set
/// where it is. Unicode identifiers are accepted because Java accepts them, even
/// though nothing on Central uses one.
fn names_a_class(text: &str) -> bool {
    let tokens: Vec<&str> = text.split('.').collect();
    if tokens.len() < 2 || !tokens.iter().copied().all(is_java_identifier) {
        return false;
    }
    tokens
        .last()
        .and_then(|token| token.chars().next())
        .is_some_and(char::is_uppercase)
}

/// Java's identifier grammar, minus the keyword check.
///
/// A token starting with a digit is what rules out every version anyone has
/// published, so this predicate is doing more work than it looks like it is.
fn is_java_identifier(token: &str) -> bool {
    let mut characters = token.chars();
    match characters.next() {
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    characters.all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Endpoint {
        Endpoint::parse(text).expect("a valid endpoint")
    }

    #[test]
    fn a_group_and_artifact_leave_the_version_open() {
        let endpoint = parsed("com.google.googlejavaformat:google-java-format");
        assert_eq!(endpoint.group_id, "com.google.googlejavaformat");
        assert_eq!(endpoint.artifact_id, "google-java-format");
        assert_eq!(endpoint.version, None);
        assert_eq!(endpoint.classifier, None);
        assert_eq!(endpoint.main_class, None);
    }

    #[test]
    fn a_third_field_is_the_version() {
        let endpoint = parsed("org.openrewrite:rewrite-maven-plugin:5.0.0");
        assert_eq!(endpoint.version.as_deref(), Some("5.0.0"));
        assert_eq!(endpoint.main_class, None);
    }

    #[test]
    fn a_fourth_field_is_the_classifier() {
        let endpoint = parsed("org.lwjgl:lwjgl:3.3.4:natives-linux");
        assert_eq!(endpoint.version.as_deref(), Some("3.3.4"));
        assert_eq!(endpoint.classifier.as_deref(), Some("natives-linux"));
        assert_eq!(endpoint.main_class, None);
    }

    #[test]
    fn an_at_sign_names_the_main_class() {
        let endpoint = parsed("g:a:1.0@com.example.Main");
        assert_eq!(endpoint.version.as_deref(), Some("1.0"));
        assert_eq!(endpoint.main_class.as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn an_at_sign_works_without_a_version() {
        let endpoint = parsed("g:a@com.example.Main");
        assert_eq!(endpoint.version, None);
        assert_eq!(endpoint.main_class.as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn an_at_sign_beats_the_heuristic() {
        // The last coordinate field looks like a class, but `@` has already
        // answered the question, so the field stays a classifier.
        let endpoint = parsed("g:a:1.0:com.example.Odd@com.example.Main");
        assert_eq!(endpoint.classifier.as_deref(), Some("com.example.Odd"));
        assert_eq!(endpoint.main_class.as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn an_at_sign_accepts_a_lowercase_or_dotless_class() {
        // The heuristic would refuse both of these; the explicit spelling exists
        // precisely so that it does not have to guess.
        assert_eq!(parsed("g:a@Main").main_class.as_deref(), Some("Main"));
        assert_eq!(
            parsed("g:a:1.0@com.example.main").main_class.as_deref(),
            Some("com.example.main")
        );
    }

    #[test]
    fn a_dotted_capitalised_last_field_is_read_as_a_main_class() {
        let endpoint = parsed("g:a:1.0:com.example.Main");
        assert_eq!(endpoint.version.as_deref(), Some("1.0"));
        assert_eq!(endpoint.classifier, None);
        assert_eq!(endpoint.main_class.as_deref(), Some("com.example.Main"));

        // The three-field form, where the alternative reading is a version.
        let endpoint = parsed("g:a:com.example.Main");
        assert_eq!(endpoint.version, None);
        assert_eq!(endpoint.main_class.as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn a_version_is_never_mistaken_for_a_main_class() {
        // Every one of these is a real published version string. A Java
        // identifier cannot start with a digit, which is what saves them all.
        for version in [
            "1.36.1",
            "4.1.100.Final",
            "5.3.0.RELEASE",
            "2.0.0-SNAPSHOT",
            "1.0.0-M1",
            "33.4.8-jre",
            "2020.0.1",
        ] {
            let endpoint = parsed(&format!("g:a:{version}"));
            assert_eq!(
                endpoint.version.as_deref(),
                Some(version),
                "{version} should stay a version"
            );
            assert_eq!(endpoint.main_class, None);
        }
    }

    #[test]
    fn a_classifier_is_never_mistaken_for_a_main_class() {
        for classifier in ["sources", "natives-linux", "linux-x86_64", "tests", "jdk15"] {
            let endpoint = parsed(&format!("g:a:1.0:{classifier}"));
            assert_eq!(
                endpoint.classifier.as_deref(),
                Some(classifier),
                "{classifier} should stay a classifier"
            );
            assert_eq!(endpoint.main_class, None);
        }
    }

    #[test]
    fn a_lowercase_final_token_is_not_a_class() {
        // `linux.x86` is a classifier shape that passes the identifier test; the
        // capitalisation rule is the only thing between it and a bad resolve.
        let endpoint = parsed("g:a:1.0:linux.x86");
        assert_eq!(endpoint.classifier.as_deref(), Some("linux.x86"));
        assert_eq!(endpoint.main_class, None);
    }

    #[test]
    fn a_two_field_endpoint_never_yields_a_main_class() {
        // Otherwise `g:Main` would resolve nothing at all, having eaten the
        // artifact id.
        let endpoint = parsed("g:com.example.Main");
        assert_eq!(endpoint.artifact_id, "com.example.Main");
        assert_eq!(endpoint.main_class, None);
    }

    #[test]
    fn positional_parsing_keeps_a_class_shaped_classifier() {
        let endpoint = Endpoint::parse_positional("g:a:1.0:com.example.Odd").expect("valid");
        assert_eq!(endpoint.classifier.as_deref(), Some("com.example.Odd"));
        assert_eq!(endpoint.main_class, None);
    }

    #[test]
    fn positional_parsing_still_honours_an_explicit_at_sign() {
        // `--main` overriding it is the CLI's business; the grammar's job is
        // only to stop *guessing*.
        let endpoint = Endpoint::parse_positional("g:a:1.0@com.example.Main").expect("valid");
        assert_eq!(endpoint.main_class.as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(parsed("  g:a:1.0  ").version.as_deref(), Some("1.0"));
    }

    #[test]
    fn a_bare_artifact_id_is_refused() {
        assert_eq!(
            Endpoint::parse("google-java-format"),
            Err(EndpointError::TooFewFields("google-java-format".to_owned()))
        );
    }

    #[test]
    fn an_empty_endpoint_is_refused() {
        assert_eq!(Endpoint::parse("   "), Err(EndpointError::Empty));
    }

    #[test]
    fn five_fields_are_refused() {
        // Rather than silently ignoring the extra one, which is how a typo in a
        // classifier turns into a mystery.
        assert!(matches!(
            Endpoint::parse("g:a:1.0:sources:extra"),
            Err(EndpointError::TooManyFields(_))
        ));
    }

    #[test]
    fn an_empty_field_is_refused() {
        // jgo reads `g:a::sources` as "no version, classifier sources"; jvx does
        // not, because a shell that expanded a variable to nothing produces
        // exactly this and should be told so.
        assert!(matches!(
            Endpoint::parse("g:a::sources"),
            Err(EndpointError::EmptyField(_))
        ));
        assert!(matches!(
            Endpoint::parse("g:a:"),
            Err(EndpointError::EmptyField(_))
        ));
    }

    #[test]
    fn two_main_classes_are_refused() {
        assert!(matches!(
            Endpoint::parse("g:a@One@Two"),
            Err(EndpointError::MultipleMainClasses(_))
        ));
    }

    #[test]
    fn a_trailing_at_sign_is_refused() {
        assert!(matches!(
            Endpoint::parse("g:a:1.0@"),
            Err(EndpointError::EmptyMainClass(_))
        ));
    }

    #[test]
    fn an_endpoint_that_is_only_a_main_class_is_refused() {
        assert!(matches!(
            Endpoint::parse("@com.example.Main"),
            Err(EndpointError::TooFewFields(_))
        ));
    }

    #[test]
    fn the_artifact_carries_the_classifier_through() {
        let endpoint = parsed("g:a:1.0:sources");
        let artifact = endpoint.artifact("1.0");
        assert_eq!(artifact.group_id, "g");
        assert_eq!(artifact.artifact_id, "a");
        assert_eq!(artifact.version, "1.0");
        assert_eq!(artifact.classifier, "sources");
        assert_eq!(artifact.extension, "jar");
    }

    #[test]
    fn coordinates_read_back_what_was_given() {
        assert_eq!(parsed("g:a").coordinates(), "g:a");
        assert_eq!(parsed("g:a:1.0").coordinates(), "g:a:1.0");
        assert_eq!(parsed("g:a:1.0:sources").coordinates(), "g:a:1.0:sources");
        assert_eq!(
            parsed("g:a:1.0@com.example.Main").coordinates(),
            "g:a:1.0",
            "the main class is not part of the coordinate"
        );
    }

    #[test]
    fn identifiers_reject_what_java_rejects() {
        assert!(is_java_identifier("Main"));
        assert!(is_java_identifier("_x$1"));
        assert!(!is_java_identifier(""));
        assert!(!is_java_identifier("1st"));
        assert!(!is_java_identifier("has-dash"));
    }
}
