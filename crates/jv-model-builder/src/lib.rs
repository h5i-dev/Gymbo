//! Turns raw POMs into effective models.
//!
//! A POM on disk is a fragment. Making it usable means resolving its parent
//! chain up to the super POM, activating and injecting profiles, merging
//! inherited values field by field, expanding `${...}`, importing BOMs, and
//! applying `<dependencyManagement>` — in an order where nearly every step
//! depends on the previous one having already run.
//!
//! # jv follows Maven 3.9, not Maven 4
//!
//! The two disagree in ways that change which artifacts get resolved, and 3.9 is
//! what users run and diff against. The differences that matter here:
//!
//! | Behaviour | Maven 3.9 (what jv does) | Maven 4 |
//! |---|---|---|
//! | Profile injection | before inheritance, per POM in the chain | after inheritance |
//! | Profile activation passes | one | two |
//! | Interpolation priority | `${project.*}` above user and POM properties | below both |
//! | Activation values | interpolated | not interpolated |
//! | `<activation><packaging>` / `<condition>` | do not exist | supported |
//! | Super POM | declares `central` | does not |
//!
//! `docs/spec/effective-pom.md` records the full pipeline and every divergence
//! found in the sources; the module docs here cite the specific upstream classes.
//!
//! # Known gaps
//!
//! Deliberate, and disclosed rather than silently wrong:
//!
//! - **Build extensions** are parsed but not loaded, so a packaging an extension
//!   contributes — `bundle`, `nbm` — has no known lifecycle bindings, and the
//!   plugins it would bind are reported as a warning instead of injected.
//! - **`<scm>` and `<distributionManagement><site>`** are not modelled, so the
//!   child-path URL appending Maven applies to them has nothing to apply to.
//!   `project.url` *is* adjusted.
//! - **`<configuration>`** is skipped entirely; nothing jv does reads it.
//!
//! # Examples
//!
//! ```
//! use jv_model_builder::{BuildContext, MapModelSource, ModelBuilder, SourcedModel};
//!
//! let mut source = MapModelSource::new();
//! source.insert(
//!     "com.example",
//!     "parent",
//!     "1.0",
//!     r#"<project>
//!          <groupId>com.example</groupId>
//!          <artifactId>parent</artifactId>
//!          <version>1.0</version>
//!          <properties><slf4j.version>2.0.9</slf4j.version></properties>
//!          <dependencyManagement><dependencies>
//!            <dependency>
//!              <groupId>org.slf4j</groupId>
//!              <artifactId>slf4j-api</artifactId>
//!              <version>${slf4j.version}</version>
//!            </dependency>
//!          </dependencies></dependencyManagement>
//!        </project>"#,
//! );
//!
//! let child = jv_model::parse_pom(
//!     r#"<project>
//!          <parent>
//!            <groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0</version>
//!          </parent>
//!          <artifactId>app</artifactId>
//!          <dependencies>
//!            <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency>
//!          </dependencies>
//!        </project>"#,
//! ).unwrap().model;
//!
//! let built = ModelBuilder::new(&source, BuildContext::empty())
//!     .build(SourcedModel::new(child, "app/pom.xml"))
//!     .unwrap();
//!
//! // The version came from the parent's management, via a parent property.
//! assert_eq!(built.model.dependencies[0].version.as_deref(), Some("2.0.9"));
//! // Coordinates and the compile default are filled in.
//! assert_eq!(built.model.group_id.as_deref(), Some("com.example"));
//! assert_eq!(built.model.dependencies[0].scope, Some(jv_model::Scope::Compile));
//! // `central` arrives from the super POM at the end of the chain.
//! assert!(built.model.repositories.iter().any(|r| r.id.as_deref() == Some("central")));
//! ```

mod activation;
mod builder;
mod context;
mod interpolate;
mod lifecycle;
mod management;
mod merge;
mod problem;
mod source;
mod validate;

pub use activation::{ActivationContext, ProfileSource, select_active_profiles};
pub use builder::{EffectiveModel, ModelBuilder, build_effective_model};
pub use context::{BuildContext, JAVA_VERSION, OS_ARCH, OS_NAME, OS_VERSION};
pub use interpolate::{Interpolator, interpolate_model, replace_ci_friendly_version};
pub use lifecycle::{inject_lifecycle_bindings, lifecycle_plugins};
pub use management::{
    extract_bom_imports, inject_default_values, inject_dependency_management,
    inject_plugin_management, merge_duplicates, merge_imported_management,
};
pub use merge::{assemble_inheritance, inject_profile};
pub use problem::{BuildError, Problem, Severity};
pub use source::{MapModelSource, ModelSource, SourcedModel};
pub use validate::{is_valid_id, is_valid_version};
