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

    /// Whether this repository would be contacted over plaintext HTTP.
    ///
    /// Maven 3.8 started shipping a mirror that blocks these, because a
    /// dependency fetched over `http://` can be replaced in transit by anyone on
    /// the path and the checksum travels the same wire. jv refuses them by
    /// default for the same reason. `localhost` is exempt: there is no wire.
    pub fn is_insecure(&self) -> bool {
        let url = self.url.to_ascii_lowercase();
        let plaintext = ["http://", "dav://", "dav:http://", "dav+http://"]
            .iter()
            .any(|scheme| url.starts_with(scheme));
        plaintext && !is_loopback(&url)
    }

    /// Whether the URL points at the local filesystem rather than a server.
    pub fn is_local(&self) -> bool {
        self.url.starts_with("file:") || self.url.starts_with('/')
    }

    /// Whether jv can speak this URL's scheme at all.
    ///
    /// Maven reaches repositories through wagons, and a build extension can add
    /// one for any scheme it likes. jetty's parent declares
    /// `mavengem:https://rubygems.org`, served by a wagon jv does not have and
    /// cannot load — it is a JVM component, not a protocol jv could implement.
    ///
    /// Such a repository has to be skipped rather than attempted. Attempting it
    /// produced `builder error for url (mavengem:...)` and failed the whole
    /// sync, so a repository jv merely cannot *use* took down artifacts every
    /// other repository could serve.
    pub fn is_supported(&self) -> bool {
        let url = self.url.to_ascii_lowercase();
        self.is_local()
            || url.starts_with("http://")
            || url.starts_with("https://")
            // Maven's WebDAV spellings resolve to plain HTTP underneath.
            || url.starts_with("dav:")
            || url.starts_with("dav+http")
            || url.starts_with("davs:")
    }
}

/// Whether a URL's host is this machine.
fn is_loopback(url: &str) -> bool {
    let Some(after_scheme) = url.split_once("://").map(|(_, rest)| rest) else {
        return false;
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // An IPv6 literal is bracketed and full of colons, so the port cannot be
    // split off before the brackets are accounted for.
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(rest, |(host, _)| host),
        None => authority
            .split_once(':')
            .map_or(authority, |(host, _)| host),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
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
    resolve_with_trust(declared, settings, Trust::Configured)
}

/// Where a repository declaration came from, which decides whether it may have
/// the user's credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// `settings.xml`, the command line, or the project the user is building —
    /// including its own parents. All under the user's control.
    Configured,
    /// A `<repositories>` block in some dependency's POM, written by whoever
    /// published it.
    ///
    /// These are still contacted, because a project whose dependency lives in a
    /// private repository has to be resolvable. They are never given
    /// credentials: a dependency four levels down declaring
    /// `<id>nexus</id><url>https://evil.example/</url>` would otherwise be handed
    /// the user's `nexus` password as HTTP Basic on the next request. Maven does
    /// not draw this line; jv does, because nothing legitimate needs it and the
    /// enterprise case — the *project's* POM naming its own repository — stays on
    /// the trusted side.
    Untrusted,
}

/// Turns declared repositories into the ones to contact, honouring where the
/// declaration came from.
pub fn resolve_with_trust(
    declared: &[Repository],
    settings: &Settings,
    trust: Trust,
) -> Vec<Repository> {
    let mut resolved: Vec<Repository> = Vec::new();

    for repository in declared {
        let mirrored = select_mirror(repository, &settings.mirrors);
        let mut effective = match mirrored {
            Some(mirror) => apply_mirror(repository, mirror),
            None => repository.clone(),
        };

        // A mirror is configured by the user, so its credentials are theirs to
        // give however the repository it stands in for was declared.
        let may_authenticate = trust == Trust::Configured || mirrored.is_some();

        // Credentials attach by the *effective* id, so a mirrored repository
        // authenticates as the mirror rather than as what the POM declared.
        if let Some(server) = settings.server(&effective.id).filter(|_| may_authenticate) {
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

/// The mirror that claims this repository.
///
/// Two passes, as `DefaultMirrorSelector.findMirror` does: a mirror whose
/// `<mirrorOf>` is exactly this repository's id wins over any pattern, wherever
/// the two sit in the file. Taking the first match in file order instead sent
/// Central's traffic — and the wildcard mirror's credentials — to the wildcard
/// host in the very ordinary configuration where `<mirrorOf>*</mirrorOf>` is
/// listed above a `<mirrorOf>central</mirrorOf>`.
fn select_mirror<'a>(repository: &Repository, mirrors: &'a [Mirror]) -> Option<&'a Mirror> {
    mirrors
        .iter()
        .find(|mirror| mirror.mirror_of.as_deref() == Some(repository.id.as_str()))
        .or_else(|| {
            mirrors
                .iter()
                .find(|mirror| mirror.matches(&repository.id, &repository.url))
        })
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
    fn a_scheme_jv_has_no_client_for_is_unsupported() {
        // jetty's parent declares this; Maven reaches it through a wagon from a
        // build extension, which jv does not load. Attempting it failed the
        // whole sync.
        assert!(!Repository::new("gems", "mavengem:https://rubygems.org").is_supported());
        assert!(!Repository::new("scp", "scp://build.example/repo").is_supported());
    }

    #[test]
    fn the_schemes_jv_can_speak_are_supported() {
        for url in [
            "https://repo.maven.apache.org/maven2",
            "http://internal.example/repo",
            "file:///opt/repo",
            "dav:https://dav.example/repo",
            "davs://dav.example/repo",
        ] {
            assert!(Repository::new("r", url).is_supported(), "{url}");
        }
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
    fn a_dependencys_repository_is_contacted_but_never_authenticated_to() {
        let settings = settings(
            r#"<settings><servers><server>
                 <id>nexus</id><username>ci</username><password>secret</password>
               </server></servers></settings>"#,
        );
        // A dependency four levels down declares an id the user has a password
        // for, pointed somewhere else entirely.
        let declared = vec![Repository::new("nexus", "https://evil.example/")];

        let trusted = resolve_with_trust(&declared, &settings, Trust::Configured);
        assert_eq!(trusted[0].credentials.password.as_deref(), Some("secret"));

        let untrusted = resolve_with_trust(&declared, &settings, Trust::Untrusted);
        // Still contacted — a project whose dependency lives in a private
        // repository has to be resolvable — but with nothing to steal.
        assert_eq!(untrusted[0].url, "https://evil.example/");
        assert!(untrusted[0].credentials.is_empty());
    }

    #[test]
    fn a_mirror_still_authenticates_however_the_repository_was_declared() {
        let settings = settings(
            r#"<settings>
                 <mirrors><mirror>
                   <id>nexus</id><url>https://nexus.corp/public</url><mirrorOf>*</mirrorOf>
                 </mirror></mirrors>
                 <servers><server>
                   <id>nexus</id><username>ci</username><password>secret</password>
                 </server></servers>
               </settings>"#,
        );
        // The mirror is the user's own configuration, so its credentials are
        // theirs to give — and every request goes there rather than to whatever
        // the dependency named.
        let resolved = resolve_with_trust(
            &[Repository::new("whatever", "https://evil.example/")],
            &settings,
            Trust::Untrusted,
        );
        assert_eq!(resolved[0].url, "https://nexus.corp/public");
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
    fn an_exact_mirror_wins_over_a_wildcard_listed_before_it() {
        let settings = settings(
            r#"<settings><mirrors>
                 <mirror><id>catch-all</id><url>https://catchall</url><mirrorOf>*</mirrorOf></mirror>
                 <mirror><id>for-central</id><url>https://central-mirror</url>
                   <mirrorOf>central</mirrorOf></mirror>
               </mirrors></settings>"#,
        );
        let resolved = resolve_repositories(&[Repository::central()], &settings);
        // Upstream matches exact ids in a first pass, so file order does not
        // decide this. Getting it wrong sends Central's traffic — and the
        // catch-all's credentials — to the wrong host.
        assert_eq!(resolved[0].id, "for-central");
        assert_eq!(resolved[0].url, "https://central-mirror");

        // A repository the exact mirror does not name still takes the wildcard.
        let other = resolve_repositories(&[Repository::new("other", "https://other")], &settings);
        assert_eq!(other[0].id, "catch-all");
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
    fn plaintext_http_is_recognised_but_loopback_is_not() {
        // A dependency fetched over http:// can be replaced in transit and its
        // checksum travels the same wire, so verifying it proves nothing. Maven
        // 3.8 blocks these; so does jv.
        for url in [
            "http://repo.corp/maven2",
            "HTTP://repo.corp/maven2",
            "dav://repo.corp/maven2",
            "dav:http://repo.corp/maven2",
            "dav+http://repo.corp/maven2",
        ] {
            assert!(Repository::new("r", url).is_insecure(), "{url}");
        }
        // No wire, no interception.
        for url in [
            "http://localhost:8081/repo",
            "http://127.0.0.1/repo",
            "http://[::1]:8081/repo",
        ] {
            assert!(!Repository::new("r", url).is_insecure(), "{url}");
        }
        // And nothing encrypted or local is affected.
        for url in [
            "https://repo.maven.apache.org/maven2",
            "file:///opt/repo",
            "dav:https://repo.corp/maven2",
        ] {
            assert!(!Repository::new("r", url).is_insecure(), "{url}");
        }
    }

    #[test]
    fn local_repositories_are_recognised() {
        assert!(Repository::new("local", "file:///opt/repo").is_local());
        assert!(!Repository::central().is_local());
    }
}
