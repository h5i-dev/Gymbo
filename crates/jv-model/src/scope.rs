//! Dependency scopes.

use std::fmt;
use std::str::FromStr;

/// A Maven dependency scope.
///
/// Scope controls both which build paths a dependency lands on and how it
/// propagates to transitive dependents. `import` is not a real scope: it is a
/// marker used only inside `<dependencyManagement>` to splice in a BOM, and it
/// never appears on a resolved node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// On every path, and propagates. The default.
    Compile,
    /// On compile and test paths, does not propagate, not packaged.
    Provided,
    /// On runtime and test paths, propagates as runtime.
    Runtime,
    /// On test paths only, does not propagate.
    Test,
    /// Like `provided`, but resolved from an explicit `systemPath` instead of a
    /// repository.
    System,
    /// `<dependencyManagement>` only: import the referenced POM's management
    /// section. Never appears on a resolved dependency.
    Import,
}

impl Scope {
    /// The scope Maven assumes when a dependency declares none.
    pub const DEFAULT: Scope = Scope::Compile;

    /// The `<scope>` spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Compile => "compile",
            Scope::Provided => "provided",
            Scope::Runtime => "runtime",
            Scope::Test => "test",
            Scope::System => "system",
            Scope::Import => "import",
        }
    }

    /// Whether a dependency in this scope is resolved from a repository.
    ///
    /// `system` dependencies come from a path on disk, and `import` entries are
    /// consumed during model building rather than resolved as artifacts.
    pub fn is_resolvable(&self) -> bool {
        !matches!(self, Scope::System | Scope::Import)
    }

    /// Whether this scope propagates to dependents at all.
    ///
    /// `provided` and `test` dependencies of a dependency are dropped; this is
    /// the coarse test, with the full parent×child table living in the resolver.
    pub fn is_transitive(&self) -> bool {
        matches!(self, Scope::Compile | Scope::Runtime)
    }
}

impl Default for Scope {
    fn default() -> Self {
        Scope::DEFAULT
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Scopes Maven 4 introduced, which Maven 3.9 does not know.
///
/// jv targets Maven 3.9 semantics, where an unrecognized scope draws a warning
/// from the validator and then behaves like `compile` because nothing downstream
/// matches its name. Recognizing these names buys a warning that says what is
/// actually going on instead of "unrecognized".
const MAVEN_4_SCOPES: &[&str] = &["compile-only", "test-only", "test-runtime", "none"];

/// Whether a scope name is one Maven 4 added.
pub fn is_maven_4_scope(name: &str) -> bool {
    MAVEN_4_SCOPES.contains(&name)
}

/// An unrecognized `<scope>` value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown dependency scope {0:?}")]
pub struct UnknownScope(pub String);

impl FromStr for Scope {
    type Err = UnknownScope;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "compile" => Ok(Scope::Compile),
            "provided" => Ok(Scope::Provided),
            "runtime" => Ok(Scope::Runtime),
            "test" => Ok(Scope::Test),
            "system" => Ok(Scope::System),
            "import" => Ok(Scope::Import),
            other => Err(UnknownScope(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for scope in [
            Scope::Compile,
            Scope::Provided,
            Scope::Runtime,
            Scope::Test,
            Scope::System,
            Scope::Import,
        ] {
            assert_eq!(scope.as_str().parse::<Scope>().unwrap(), scope);
        }
    }

    #[test]
    fn unknown_scope_is_rejected() {
        assert!("Compile".parse::<Scope>().is_err());
        assert!("".parse::<Scope>().is_err());
    }

    #[test]
    fn default_is_compile() {
        assert_eq!(Scope::default(), Scope::Compile);
    }
}
