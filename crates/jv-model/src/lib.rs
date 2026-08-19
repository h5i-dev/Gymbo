//! Maven's POM model and its parsers.
//!
//! This crate is the data layer: it knows what a POM *says*, not what it
//! *means*. Turning a POM into an effective model — resolving the parent chain,
//! interpolating properties, activating profiles, importing BOMs — is
//! `jv-model-builder`'s job, and resolving dependencies is `jv-resolver`'s.
//!
//! The split matters because a raw POM is a fragment. Fields are `Option` even
//! where Maven always ends up with a value, because "absent" and "explicitly
//! set" behave differently once inheritance and `<dependencyManagement>` get
//! involved, and collapsing them at parse time loses information that cannot be
//! recovered later.
//!
//! See `docs/spec/pom-model.md` for the schema this mirrors and the parts
//! deliberately omitted.
//!
//! # Examples
//!
//! ```
//! use jv_model::{Scope, TypeRegistry, parse_pom};
//!
//! let pom = parse_pom(r#"
//!     <project>
//!       <groupId>com.example</groupId>
//!       <artifactId>demo</artifactId>
//!       <version>1.0</version>
//!       <dependencies>
//!         <dependency>
//!           <groupId>com.example</groupId>
//!           <artifactId>helper</artifactId>
//!           <version>2.0</version>
//!           <type>test-jar</type>
//!           <scope>test</scope>
//!         </dependency>
//!       </dependencies>
//!     </project>
//! "#).unwrap();
//!
//! let dependency = &pom.model.dependencies[0];
//! assert_eq!(dependency.scope, Some(Scope::Test));
//!
//! // The declared type expands into an extension and a classifier.
//! let artifact = dependency.to_artifact(&TypeRegistry::new());
//! assert_eq!(artifact.extension, "jar");
//! assert_eq!(artifact.classifier, "tests");
//! assert_eq!(artifact.file_name(), "helper-2.0-tests.jar");
//! ```

mod coordinates;
mod metadata;
mod model;
mod parse;
mod scope;
pub mod security;
mod settings;
pub mod toolchains;
mod types;

pub use coordinates::{
    Artifact, DEFAULT_EXTENSION, DEFAULT_TYPE, Dependency, Exclusion, Ga, ManagementKey, SNAPSHOT,
    base_version_of, is_snapshot_version,
};
pub use metadata::{
    Metadata, PluginMapping, Snapshot, SnapshotVersion, Versioning, parse_metadata,
};
pub use model::{
    Activation, ActivationFile, ActivationOs, ActivationProperty, Build, DEFAULT_PACKAGING,
    DEFAULT_PARENT_RELATIVE_PATH, DEFAULT_PLUGIN_GROUP_ID, DistributionManagement, Extension,
    Model, Parent, Plugin, PluginExecution, Prerequisites, Profile, Properties, Relocation,
    Repository, RepositoryPolicy,
};
pub use parse::{ParseError, ParsedPom, parse_pom};
pub use scope::{Scope, UnknownScope, is_maven_4_scope};
pub use settings::{Mirror, Proxy, Server, Settings, SettingsProfile, parse_settings};
pub use types::{TypeDescriptor, TypeRegistry};
