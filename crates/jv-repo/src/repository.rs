//! Which repositories to ask, in what order, with what credentials.
//!
//! A POM's `<repositories>` are not the list jv actually contacts. `settings.xml`
//! rewrites them through `<mirrors>`, attaches credentials by id, and can block
//! them outright. Getting this wrong is not a subtle failure: in a corporate
//! build every request goes to a Nexus or Artifactory that only the mirror
//! configuration knows about, and skipping it means nothing resolves.

use jv_model::{Mirror, Repository as RepositoryModel, RepositoryPolicy as PolicyModel, Settings};

use crate::policy::{ChecksumPolicy, Policy, UpdatePolicy};

/// Maven Central, which every POM inherits from the super POM.
pub const CENTRAL_ID: &str = "central";
pub const CENTRAL_URL: &str = "https://repo.maven.apache.org/maven2";

/// Credentials for one repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub username: Option<String>,
    pub password: Option<String>,
}

impl Credentials {
    pub fn is_empty(&self) -> bool {
        self.username.is_none() && self.password.is_none()
    }
}

/// A repository jv will actually contact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub id: String,
    pub url: String,
    /// How releases are cached and verified.
    pub releases: Policy,
    /// How snapshots are cached and verified. Central disables them entirely.
    pub snapshots: Policy,
    pub credentials: Credentials,
    /// A mirror may declare a repository unreachable rather than redirect it.
    pub blocked: bool,
}

impl Repository {
    /// A repository with Maven's default policies.
    pub fn new(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            releases: Policy::default(),
            snapshots: Policy::default(),
            credentials: Credentials::default(),
            blocked: false,
        }
    }

    /// Maven Central as the super POM declares it: releases only.
    pub fn central() -> Self {
        Self {
            snapshots: Policy {
                enabled: false,
                ..Policy::default()
            },
            ..Self::new(CENTRAL_ID, CENTRAL_URL)
        }
    }

    /// The policy that governs a version, chosen by whether it is a snapshot.
    pub fn policy_for(&self, version: &str) -> &Policy {
        if jv_model::is_snapshot_version(version) {
            &self.snapshots
        } else {
            &self.releases
        }
    }

    /// Whether this repository is worth asking for a version at all.
    pub fn accepts(&self, version: &str) -> bool {
        !self.blocked && self.policy_for(version).enabled
    }

    /// Whether the URL points at the local filesystem rather than a server.
    pub fn is_local(&self) -> bool {
        self.url.starts_with("file:") || self.url.starts_with('/')
    }
}

/// Turns a POM's declared repositories into the ones to contact.
///
/// # Examples
///
/// ```
/// use jv_model::parse_settings;
/// use jv_repo::{Repository, resolve_repositories};
///
/// let settings = parse_settings(r#"
///     <settings>
///       <mirrors><mirror>
///         <id>nexus</id>
///         <url>https://nexus.corp/public</url>
///         <mirrorOf>*</mirrorOf>
///       </mirror></mirrors>
///       <servers><server>
///         <id>nexus</id><username>ci</username><password>secret</password>
///       </server></servers>
///     </settings>
/// "#).unwrap();
///
/// let resolved = resolve_repositories(&[Repository::central()], &settings);
/// // Central is gone; every request goes to the mirror, with its credentials.
/// assert_eq!(resolved.len(), 1);
/// assert_eq!(resolved[0].id, "nexus");
/// assert_eq!(resolved[0].credentials.username.as_deref(), Some("ci"));
/// ```
pub fn resolve_repositories(declared: &[Repository], settings: &Settings) -> Vec<Repository> {
    let mut resolved: Vec<Repository> = Vec::new();

    for repository in declared {
        let mut effective = match select_mirror(repository, &settings.mirrors) {
            Some(mirror) => apply_mirror(repository, mirror),
            None => repository.clone(),
        };

        // Credentials attach by the *effective* id, so a mirrored repository
        // authenticates as the mirror rather than as what the POM declared.
        if let Some(server) = settings.server(&effective.id) {
            effective.credentials = Credentials {
                username: server.username.clone(),
                password: if server.has_encrypted_password() {
                    // Sending ciphertext produces a baffling 401; saying so is
                    // more useful than trying.
                    None
                } else {
                    server.password.clone()
                },
            };
        }

        // Two declarations that mirror to the same place are one repository.
        if let Some(existing) = resolved.iter_mut().find(|other| other.id == effective.id) {
            existing.releases = existing.releases.merge(&effective.releases);
            existing.snapshots = existing.snapshots.merge(&effective.snapshots);
            continue;
        }
        resolved.push(effective);
    }

    resolved
}

/// The first mirror that claims this repository.
fn select_mirror<'a>(repository: &Repository, mirrors: &'a [Mirror]) -> Option<&'a Mirror> {
    mirrors
        .iter()
        .find(|mirror| mirror.matches(&repository.id, &repository.url))
}

fn apply_mirror(repository: &Repository, mirror: &Mirror) -> Repository {
    Repository {
        id: mirror.id.clone().unwrap_or_else(|| repository.id.clone()),
        url: mirror.url.clone().unwrap_or_else(|| repository.url.clone()),
        // The mirrored repository keeps the original's policies: a mirror
        // redirects where requests go, not which versions are acceptable.
        releases: repository.releases.clone(),
        snapshots: repository.snapshots.clone(),
        credentials: Credentials::default(),
        blocked: mirror.is_blocked(),
    }
}

/// Converts a POM's `<repository>` into one jv can contact.
pub fn from_model(model: &RepositoryModel) -> Option<Repository> {
    let url = model.url.clone()?;
    let id = model.id.clone().unwrap_or_else(|| url.clone());
    Some(Repository {
        id,
        url,
        releases: policy_from(model.releases.as_ref(), false),
        snapshots: policy_from(model.snapshots.as_ref(), true),
        credentials: Credentials::default(),
        blocked: false,
    })
}

/// Reads a `<releases>` or `<snapshots>` block, applying Maven's defaults.
fn policy_from(model: Option<&PolicyModel>, _snapshots: bool) -> Policy {
    let Some(model) = model else {
        return Policy::default();
    };
    Policy {
        enabled: model.enabled.unwrap_or(true),
        update: model
            .update_policy
            .as_deref()
            .map(UpdatePolicy::parse)
            .unwrap_or_default(),
        checksum: model
            .checksum_policy
            .as_deref()
            .map(ChecksumPolicy::parse)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::parse_settings;

    fn settings(xml: &str) -> Settings {
        parse_settings(xml).expect("settings parse")
    }

    #[test]
    fn without_mirrors_nothing_changes() {
        let declared = vec![Repository::central()];
        let resolved = resolve_repositories(&declared, &Settings::default());
        assert_eq!(resolved, declared);
    }

    #[test]
    fn a_wildcard_mirror_replaces_everything() {
        let settings = settings(
            r#"<settings><mirrors><mirror>
                 <id>nexus</id><url>https://nexus.corp/public</url><mirrorOf>*</mirrorOf>
               </mirror></mirrors></settings>"#,
        );
        let declared = vec![
            Repository::central(),
            Repository::new("other", "https://other.example/repo"),
        ];
        let resolved = resolve_repositories(&declared, &settings);
        // Both collapse onto the mirror, which is one repository.
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "nexus");
        assert_eq!(resolved[0].url, "https://nexus.corp/public");
    }

    #[test]
    fn an_excluded_repository_keeps_its_own_url() {
        let settings = settings(
            r#"<settings><mirrors><mirror>
                 <id>nexus</id><url>https://nexus.corp/public</url>
                 <mirrorOf>*,!internal</mirrorOf>
               </mirror></mirrors></settings>"#,
        );
        let declared = vec![
            Repository::central(),
            Repository::new("internal", "https://internal.corp/repo"),
        ];
        let resolved = resolve_repositories(&declared, &settings);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].id, "nexus");
        assert_eq!(resolved[1].id, "internal");
        assert_eq!(resolved[1].url, "https://internal.corp/repo");
    }

    #[test]
    fn credentials_attach_to_the_effective_id() {
        let settings = settings(
            r#"<settings>
                 <mirrors><mirror>
                   <id>nexus</id><url>https://nexus.corp/public</url><mirrorOf>*</mirrorOf>
                 </mirror></mirrors>
                 <servers>
                   <server><id>nexus</id><username>ci</username><password>secret</password></server>
                   <server><id>central</id><username>wrong</username></server>
                 </servers>
               </settings>"#,
        );
        let resolved = resolve_repositories(&[Repository::central()], &settings);
        // The mirror's credentials, not the mirrored repository's.
        assert_eq!(resolved[0].credentials.username.as_deref(), Some("ci"));
        assert_eq!(resolved[0].credentials.password.as_deref(), Some("secret"));
    }

    #[test]
    fn an_encrypted_password_is_withheld_rather_than_sent() {
        let settings = settings(
            r#"<settings><servers><server>
                 <id>central</id><username>ci</username>
                 <password>{jSMOWnoPFgsHVpMvz5VrIt5kRbzGpI8u+9EF1iFQyJQ=}</password>
               </server></servers></settings>"#,
        );
        let resolved = resolve_repositories(&[Repository::central()], &settings);
        assert_eq!(resolved[0].credentials.username.as_deref(), Some("ci"));
        // Sending ciphertext would authenticate as nobody and report a 401.
        assert_eq!(resolved[0].credentials.password, None);
    }

    #[test]
    fn a_blocked_mirror_makes_a_repository_unreachable() {
        let settings = settings(
            r#"<settings><mirrors><mirror>
                 <id>blocked</id><url>https://unused</url>
                 <mirrorOf>*</mirrorOf><blocked>true</blocked>
               </mirror></mirrors></settings>"#,
        );
        let resolved = resolve_repositories(&[Repository::central()], &settings);
        assert!(resolved[0].blocked);
        assert!(!resolved[0].accepts("1.0"));
    }

    #[test]
    fn central_refuses_snapshots() {
        let central = Repository::central();
        assert!(central.accepts("1.0"));
        assert!(!central.accepts("1.0-SNAPSHOT"));
        // A plain repository takes both.
        let other = Repository::new("other", "https://example/repo");
        assert!(other.accepts("1.0"));
        assert!(other.accepts("1.0-SNAPSHOT"));
    }

    #[test]
    fn policies_come_from_the_pom() {
        let pom = jv_model::parse_pom(
            r#"<project><repositories><repository>
                 <id>r</id><url>https://example/repo</url>
                 <releases><updatePolicy>never</updatePolicy><checksumPolicy>fail</checksumPolicy></releases>
                 <snapshots><enabled>false</enabled></snapshots>
               </repository></repositories></project>"#,
        )
        .unwrap();
        let repository = from_model(&pom.model.repositories[0]).expect("a url");
        assert_eq!(repository.releases.update, UpdatePolicy::Never);
        assert_eq!(repository.releases.checksum, ChecksumPolicy::Fail);
        assert!(!repository.snapshots.enabled);
        assert!(repository.releases.enabled);
    }

    #[test]
    fn a_repository_without_a_url_is_unusable() {
        let pom = jv_model::parse_pom(
            r#"<project><repositories><repository><id>r</id></repository></repositories></project>"#,
        )
        .unwrap();
        assert!(from_model(&pom.model.repositories[0]).is_none());
    }

    #[test]
    fn local_repositories_are_recognised() {
        assert!(Repository::new("local", "file:///opt/repo").is_local());
        assert!(!Repository::central().is_local());
    }
}
