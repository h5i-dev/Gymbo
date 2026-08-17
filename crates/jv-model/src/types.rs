//! The dependency `<type>` table.
//!
//! A dependency's type is a shorthand that expands into a file extension, a
//! possible classifier, and two facts the resolver needs: whether the artifact
//! already bundles its dependencies (in which case the resolver stops
//! descending into it), and whether it belongs on the Java build path.
//!
//! Ported from `org.apache.maven.impl.resolver.type.DefaultTypeProvider` and
//! `DefaultType` (which is where Maven 4 keeps what Maven 3 spelled out in
//! `artifact-handlers.xml`; the entries shared by both agree).

use std::collections::HashMap;

/// What a `<type>` expands to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeDescriptor<'a> {
    /// The type name as declared.
    pub id: &'a str,
    /// The artifact's file extension.
    pub extension: &'a str,
    /// The classifier the type implies, empty when it implies none. An
    /// explicitly declared `<classifier>` takes precedence over this.
    pub classifier: &'a str,
    /// Whether the artifact bundles its own dependencies.
    ///
    /// True for the container formats (`war`, `ear`, `rar`, `par`). The resolver
    /// does not traverse into such a dependency, because its contents are
    /// already inside the archive — this is what upstream's
    /// `FatArtifactTraverser` enforces.
    pub includes_dependencies: bool,
    /// Whether the artifact belongs on the Java class path.
    ///
    /// Upstream derives this from the type declaring the `CLASSES` path type.
    pub constitutes_build_path: bool,
}

/// One row of the static table.
struct Row {
    id: &'static str,
    extension: &'static str,
    classifier: &'static str,
    includes_dependencies: bool,
    constitutes_build_path: bool,
}

/// The built-in types.
///
/// `bom`, `modular-jar`, `classpath-jar`, `fatjar`, `test-java-source` and the
/// processor types are Maven 4 additions. They are listed because recognizing
/// them is harmless and strictly better than the unknown-type fallback, which is
/// what Maven 3.9 would apply to them.
const TABLE: &[Row] = &[
    Row {
        id: "pom",
        extension: "pom",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "bom",
        extension: "pom",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "maven-plugin",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: true,
    },
    Row {
        id: "jar",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: true,
    },
    Row {
        id: "javadoc",
        extension: "jar",
        classifier: "javadoc",
        includes_dependencies: false,
        constitutes_build_path: true,
    },
    Row {
        id: "java-source",
        extension: "jar",
        classifier: "sources",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "test-jar",
        extension: "jar",
        classifier: "tests",
        includes_dependencies: false,
        constitutes_build_path: true,
    },
    Row {
        id: "test-java-source",
        extension: "jar",
        classifier: "test-sources",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "modular-jar",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "classpath-jar",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: true,
    },
    Row {
        id: "fatjar",
        extension: "jar",
        classifier: "",
        includes_dependencies: true,
        constitutes_build_path: true,
    },
    Row {
        id: "processor",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "classpath-processor",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "modular-processor",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: false,
    },
    Row {
        id: "ejb",
        extension: "jar",
        classifier: "",
        includes_dependencies: false,
        constitutes_build_path: true,
    },
    Row {
        id: "ejb-client",
        extension: "jar",
        classifier: "client",
        includes_dependencies: false,
        constitutes_build_path: true,
    },
    Row {
        id: "war",
        extension: "war",
        classifier: "",
        includes_dependencies: true,
        constitutes_build_path: false,
    },
    Row {
        id: "ear",
        extension: "ear",
        classifier: "",
        includes_dependencies: true,
        constitutes_build_path: false,
    },
    Row {
        id: "rar",
        extension: "rar",
        classifier: "",
        includes_dependencies: true,
        constitutes_build_path: false,
    },
    Row {
        id: "par",
        extension: "par",
        classifier: "",
        includes_dependencies: true,
        constitutes_build_path: false,
    },
];

/// A type declared by a build extension rather than built in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CustomType {
    extension: String,
    classifier: String,
    includes_dependencies: bool,
    constitutes_build_path: bool,
}

/// Resolves `<type>` names to their expansion.
///
/// Build extensions can contribute types in Maven; jv does not load extensions
/// yet, so [`TypeRegistry::register`] exists for tests and for the day it does.
#[derive(Clone, Debug, Default)]
pub struct TypeRegistry {
    custom: HashMap<String, CustomType>,
}

impl TypeRegistry {
    /// A registry holding only the built-in types.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or overrides a type.
    pub fn register(
        &mut self,
        id: impl Into<String>,
        extension: impl Into<String>,
        classifier: impl Into<String>,
        includes_dependencies: bool,
        constitutes_build_path: bool,
    ) {
        self.custom.insert(
            id.into(),
            CustomType {
                extension: extension.into(),
                classifier: classifier.into(),
                includes_dependencies,
                constitutes_build_path,
            },
        );
    }

    /// Expands a type name.
    ///
    /// An unrecognized type behaves the way Maven's default artifact handler
    /// does: the type *is* the extension, with no classifier, not bundling
    /// dependencies and not on the build path. This is why `<type>tar.gz</type>`
    /// works without anyone declaring it.
    pub fn get<'a>(&'a self, id: &'a str) -> TypeDescriptor<'a> {
        if let Some(custom) = self.custom.get(id) {
            return TypeDescriptor {
                id,
                extension: &custom.extension,
                classifier: &custom.classifier,
                includes_dependencies: custom.includes_dependencies,
                constitutes_build_path: custom.constitutes_build_path,
            };
        }
        for row in TABLE {
            if row.id == id {
                return TypeDescriptor {
                    id,
                    extension: row.extension,
                    classifier: row.classifier,
                    includes_dependencies: row.includes_dependencies,
                    constitutes_build_path: row.constitutes_build_path,
                };
            }
        }
        TypeDescriptor {
            id,
            extension: id,
            classifier: "",
            includes_dependencies: false,
            constitutes_build_path: false,
        }
    }

    /// Whether the type is one of the built-in ones or was registered.
    pub fn is_known(&self, id: &str) -> bool {
        self.custom.contains_key(id) || TABLE.iter().any(|row| row.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_jar() {
        let registry = TypeRegistry::new();
        let jar = registry.get("jar");
        assert_eq!(jar.extension, "jar");
        assert_eq!(jar.classifier, "");
        assert!(!jar.includes_dependencies);
        assert!(jar.constitutes_build_path);
    }

    #[test]
    fn types_that_imply_a_classifier() {
        let registry = TypeRegistry::new();
        assert_eq!(registry.get("test-jar").classifier, "tests");
        assert_eq!(registry.get("javadoc").classifier, "javadoc");
        assert_eq!(registry.get("java-source").classifier, "sources");
        assert_eq!(registry.get("ejb-client").classifier, "client");
        // All of those are still jars on disk.
        for id in ["test-jar", "javadoc", "java-source", "ejb-client"] {
            assert_eq!(registry.get(id).extension, "jar", "{id}");
        }
    }

    #[test]
    fn container_formats_bundle_their_dependencies() {
        let registry = TypeRegistry::new();
        for id in ["war", "ear", "rar", "par"] {
            let descriptor = registry.get(id);
            assert!(descriptor.includes_dependencies, "{id}");
            assert_eq!(descriptor.extension, id);
            // A bundled archive is not itself a class path entry.
            assert!(!descriptor.constitutes_build_path, "{id}");
        }
    }

    #[test]
    fn pom_is_not_on_the_build_path() {
        let registry = TypeRegistry::new();
        let pom = registry.get("pom");
        assert_eq!(pom.extension, "pom");
        assert!(!pom.constitutes_build_path);
        // A BOM is a POM on disk.
        assert_eq!(registry.get("bom").extension, "pom");
    }

    #[test]
    fn unknown_type_becomes_its_own_extension() {
        let registry = TypeRegistry::new();
        let descriptor = registry.get("tar.gz");
        assert_eq!(descriptor.extension, "tar.gz");
        assert_eq!(descriptor.classifier, "");
        assert!(!descriptor.includes_dependencies);
        assert!(!descriptor.constitutes_build_path);
        assert!(!registry.is_known("tar.gz"));
    }

    #[test]
    fn registered_types_win() {
        let mut registry = TypeRegistry::new();
        registry.register("nar", "nar", "amd64", true, false);
        let descriptor = registry.get("nar");
        assert_eq!(descriptor.extension, "nar");
        assert_eq!(descriptor.classifier, "amd64");
        assert!(descriptor.includes_dependencies);
        assert!(registry.is_known("nar"));
    }
}
