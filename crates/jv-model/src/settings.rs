//! `settings.xml`: the user's and installation's repository configuration.
//!
//! Mirrors `api/maven-api-settings/src/main/mdo/settings.mdo`.
//!
//! Enterprise CI is the reason this matters. Nearly every corporate build routes
//! Central through a Nexus or Artifactory mirror, so a tool that ignores
//! `<mirrors>` and `<servers>` cannot resolve anything there — which would make
//! `jv sync`, the adoption path into CI, useless exactly where it is needed.

use std::fmt;

use crate::model::{Activation, Properties, Repository};
use crate::parse::{ParseError, XmlParser};

/// A parsed `settings.xml`.
///
/// Maven reads two of these — the installation's and the user's — and merges
/// them, with the user's winning. Merging is `jv-repo`'s job; this type is one
/// file's contents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// Overrides `~/.m2/repository`.
    pub local_repository: Option<String>,
    pub interactive_mode: Option<bool>,
    pub offline: Option<bool>,
    pub proxies: Vec<Proxy>,
    pub servers: Vec<Server>,
    pub mirrors: Vec<Mirror>,
    /// Deprecated by Maven in favour of declaring repositories in a profile, but
    /// still read.
    pub repositories: Vec<Repository>,
    pub plugin_repositories: Vec<Repository>,
    pub profiles: Vec<SettingsProfile>,
    /// Profile ids activated unconditionally.
    pub active_profiles: Vec<String>,
    pub plugin_groups: Vec<String>,
}

impl Settings {
    /// The credentials configured for a repository id.
    pub fn server(&self, id: &str) -> Option<&Server> {
        self.servers
            .iter()
            .find(|server| server.id.as_deref() == Some(id))
    }

    /// Whether the settings ask for offline operation.
    pub fn is_offline(&self) -> bool {
        self.offline.unwrap_or(false)
    }
}

/// An HTTP proxy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Proxy {
    pub id: Option<String>,
    /// Defaults to true when absent.
    pub active: Option<bool>,
    pub protocol: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// A `|`-separated list of hosts to bypass, where `*` is a wildcard.
    pub non_proxy_hosts: Option<String>,
}

impl Proxy {
    pub fn is_active(&self) -> bool {
        self.active.unwrap_or(true)
    }
}

/// Credentials for a repository id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Server {
    pub id: Option<String>,
    pub username: Option<String>,
    /// May be encrypted as `{...}`, which requires `settings-security.xml`.
    /// [`crate::security`] decrypts it at the point settings are loaded, so
    /// this is normally plaintext by the time anything reads it; a value that
    /// stays encrypted is withheld rather than sent.
    pub password: Option<String>,
    pub private_key: Option<String>,
    pub passphrase: Option<String>,
}

impl Server {
    /// Whether the password is in Maven's encrypted form.
    ///
    /// Recognizing this is what lets jv fail with "encrypted passwords are not
    /// supported yet" instead of authenticating with ciphertext and reporting a
    /// baffling 401.
    pub fn has_encrypted_password(&self) -> bool {
        self.password
            .as_deref()
            .is_some_and(|p| p.trim_start().starts_with('{') && p.trim_end().ends_with('}'))
    }
}

/// A mirror that stands in for one or more repositories.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mirror {
    pub id: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    /// Which repository ids this mirrors. See [`Mirror::matches`].
    pub mirror_of: Option<String>,
    pub layout: Option<String>,
    pub mirror_of_layouts: Option<String>,
    /// A blocked mirror makes matching repositories unreachable rather than
    /// redirecting them.
    pub blocked: Option<bool>,
}

impl Mirror {
    pub fn is_blocked(&self) -> bool {
        self.blocked.unwrap_or(false)
    }

    /// Whether this mirror stands in for the given repository.
    ///
    /// `mirrorOf` is a comma-separated list of repository ids with three
    /// wildcards: `*` matches every repository, `external:*` every repository
    /// that is not on localhost or a `file:` URL, and `external:http:*` the same
    /// but only for insecure HTTP.
    ///
    /// Ported from `DefaultMirrorSelector.matchPattern`, whose details are
    /// easy to get wrong and all load-bearing:
    ///
    /// * **Entries are not trimmed.** `central, jcenter` has a second entry of
    ///   `" jcenter"`, which matches no repository. Trimming it — which reads
    ///   like a kindness — silently redirects a repository Maven leaves alone.
    /// * **`!` negates an id and only an id.** `!external:*` excludes a
    ///   repository literally named `external:*`; it is not a negated wildcard.
    ///   So `*,!external:*` mirrors everything, localhost included.
    /// * **An exact match stops the scan; a wildcard does not**, so a later
    ///   negation can still veto a wildcard but not an exact match. That is why
    ///   `central,!central` mirrors `central`.
    pub fn matches(&self, repository_id: &str, repository_url: &str) -> bool {
        let Some(pattern) = self.mirror_of.as_deref() else {
            return false;
        };
        // Upstream short-circuits the whole pattern before splitting it, which
        // is what makes a lone `*` or a lone id match without the list rules
        // applying at all.
        if pattern == "*" || pattern == repository_id {
            return true;
        }

        let mut matched = false;
        for entry in pattern.split(',') {
            if entry.len() > 1 && entry.starts_with('!') {
                if &entry[1..] == repository_id {
                    // Explicitly excluded, and nothing later can undo it.
                    return false;
                }
            } else if entry == repository_id {
                return true;
            } else {
                // The wildcards do not stop the scan, so a later negation can
                // still veto one. That asymmetry with an exact match is
                // upstream's and is the whole reason `*,!internal` works.
                matched |= match entry {
                    "*" => true,
                    "external:*" => is_external(repository_url),
                    "external:http:*" => is_external_http(repository_url),
                    _ => false,
                };
            }
        }
        matched
    }
}

/// Whether a repository lives outside this machine.
fn is_external(url: &str) -> bool {
    !is_local_host(url) && !protocol_of(url).eq_ignore_ascii_case("file")
}

/// Whether a repository is external *and* reached over plain HTTP.
///
/// The four protocols are upstream's: WebDAV over HTTP is spelled three
/// different ways in the wild and all three are as insecure as `http`.
fn is_external_http(url: &str) -> bool {
    let protocol = protocol_of(url);
    ["http", "dav", "dav:http", "dav+http"]
        .iter()
        .any(|known| protocol.eq_ignore_ascii_case(known))
        && !is_local_host(url)
}

fn is_local_host(url: &str) -> bool {
    matches!(host_of(url).as_deref(), Some("localhost" | "127.0.0.1"))
}

/// The protocol of a repository URL.
///
/// Not simply "everything before the first colon": Maven's own URL pattern
/// allows a second segment when it is followed by `://`, so `dav:http://host`
/// has the protocol `dav:http` rather than `dav`. That spelling is exactly the
/// one `external:http:*` has to recognise.
fn protocol_of(url: &str) -> &str {
    let Some((head, _)) = url.split_once("://") else {
        return url.split_once(':').map_or("", |(scheme, _)| scheme);
    };
    head
}

/// The host component of a URL, without pulling in a URL parser for what is a
/// two-delimiter lookup.
fn host_of(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Drop userinfo and port.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = authority.split_once(':').map_or(authority, |(h, _)| h);
    (!host.is_empty()).then(|| host.to_owned())
}

/// A profile declared in `settings.xml`.
///
/// Narrower than a POM profile: it may only contribute properties and
/// repositories, never dependencies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsProfile {
    pub id: Option<String>,
    pub activation: Option<Activation>,
    pub properties: Properties,
    pub repositories: Vec<Repository>,
    pub plugin_repositories: Vec<Repository>,
}

impl fmt::Display for Mirror {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} -> {} (mirrorOf {})",
            self.id.as_deref().unwrap_or("[no id]"),
            self.url.as_deref().unwrap_or("[no url]"),
            self.mirror_of.as_deref().unwrap_or("[nothing]")
        )
    }
}

/// Parses a `settings.xml`.
///
/// # Examples
///
/// ```
/// use jv_model::parse_settings;
///
/// let settings = parse_settings(r#"
///     <settings>
///       <mirrors>
///         <mirror>
///           <id>nexus</id>
///           <url>https://nexus.corp/repository/maven-public</url>
///           <mirrorOf>*,!internal</mirrorOf>
///         </mirror>
///       </mirrors>
///     </settings>
/// "#).unwrap();
///
/// let mirror = &settings.mirrors[0];
/// assert!(mirror.matches("central", "https://repo.maven.apache.org/maven2"));
/// // The exclusion wins over the wildcard.
/// assert!(!mirror.matches("internal", "https://nexus.corp/internal"));
/// ```
pub fn parse_settings(xml: &str) -> Result<Settings, ParseError> {
    let mut parser = XmlParser::new(xml);
    parser.enter_root("settings")?;
    let mut settings = Settings::default();
    parser.children("settings", |parser, name| match name {
        b"localRepository" => parser.text_into(&mut settings.local_repository),
        b"interactiveMode" => parser.bool_into(&mut settings.interactive_mode),
        b"offline" => parser.bool_into(&mut settings.offline),
        b"proxies" => {
            settings.proxies = parser.list("proxies", b"proxy", parse_proxy)?;
            Ok(true)
        }
        b"servers" => {
            settings.servers = parser.list("servers", b"server", parse_server)?;
            Ok(true)
        }
        b"mirrors" => {
            settings.mirrors = parser.list("mirrors", b"mirror", parse_mirror)?;
            Ok(true)
        }
        b"repositories" => {
            settings.repositories =
                parser.list("repositories", b"repository", XmlParser::parse_repository)?;
            Ok(true)
        }
        b"pluginRepositories" => {
            settings.plugin_repositories = parser.list(
                "pluginRepositories",
                b"pluginRepository",
                XmlParser::parse_repository,
            )?;
            Ok(true)
        }
        b"profiles" => {
            settings.profiles = parser.list("profiles", b"profile", parse_settings_profile)?;
            Ok(true)
        }
        b"activeProfiles" => {
            settings.active_profiles = parser.text_list("activeProfiles", b"activeProfile")?;
            Ok(true)
        }
        b"pluginGroups" => {
            settings.plugin_groups = parser.text_list("pluginGroups", b"pluginGroup")?;
            Ok(true)
        }
        _ => Ok(false),
    })?;
    Ok(settings)
}

fn parse_proxy(parser: &mut XmlParser<'_>) -> Result<Proxy, ParseError> {
    let mut proxy = Proxy::default();
    parser.children("proxy", |parser, name| match name {
        b"id" => parser.text_into(&mut proxy.id),
        b"active" => parser.bool_into(&mut proxy.active),
        b"protocol" => parser.text_into(&mut proxy.protocol),
        b"host" => parser.text_into(&mut proxy.host),
        b"port" => parser.text_into(&mut proxy.port),
        b"username" => parser.text_into(&mut proxy.username),
        b"password" => parser.text_into(&mut proxy.password),
        b"nonProxyHosts" => parser.text_into(&mut proxy.non_proxy_hosts),
        _ => Ok(false),
    })?;
    Ok(proxy)
}

fn parse_server(parser: &mut XmlParser<'_>) -> Result<Server, ParseError> {
    let mut server = Server::default();
    parser.children("server", |parser, name| match name {
        b"id" => parser.text_into(&mut server.id),
        b"username" => parser.text_into(&mut server.username),
        b"password" => parser.text_into(&mut server.password),
        b"privateKey" => parser.text_into(&mut server.private_key),
        b"passphrase" => parser.text_into(&mut server.passphrase),
        _ => Ok(false),
    })?;
    Ok(server)
}

fn parse_mirror(parser: &mut XmlParser<'_>) -> Result<Mirror, ParseError> {
    let mut mirror = Mirror::default();
    parser.children("mirror", |parser, name| match name {
        b"id" => parser.text_into(&mut mirror.id),
        b"name" => parser.text_into(&mut mirror.name),
        b"url" => parser.text_into(&mut mirror.url),
        b"mirrorOf" => parser.text_into(&mut mirror.mirror_of),
        b"layout" => parser.text_into(&mut mirror.layout),
        b"mirrorOfLayouts" => parser.text_into(&mut mirror.mirror_of_layouts),
        b"blocked" => parser.bool_into(&mut mirror.blocked),
        _ => Ok(false),
    })?;
    Ok(mirror)
}

fn parse_settings_profile(parser: &mut XmlParser<'_>) -> Result<SettingsProfile, ParseError> {
    let mut profile = SettingsProfile::default();
    parser.children("profile", |parser, name| match name {
        b"id" => parser.text_into(&mut profile.id),
        b"activation" => {
            profile.activation = Some(parser.parse_activation()?);
            Ok(true)
        }
        b"properties" => {
            parser.parse_properties(&mut profile.properties)?;
            Ok(true)
        }
        b"repositories" => {
            profile.repositories =
                parser.list("repositories", b"repository", XmlParser::parse_repository)?;
            Ok(true)
        }
        b"pluginRepositories" => {
            profile.plugin_repositories = parser.list(
                "pluginRepositories",
                b"pluginRepository",
                XmlParser::parse_repository,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    })?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_settings() {
        let settings = parse_settings(
            r#"<settings>
                 <localRepository>/opt/m2</localRepository>
                 <offline>true</offline>
                 <servers>
                   <server><id>nexus</id><username>ci</username><password>secret</password></server>
                 </servers>
                 <mirrors>
                   <mirror><id>nexus</id><url>https://nexus.corp/public</url><mirrorOf>*</mirrorOf></mirror>
                 </mirrors>
                 <proxies>
                   <proxy><id>p</id><host>proxy.corp</host><port>3128</port>
                     <nonProxyHosts>*.corp|localhost</nonProxyHosts></proxy>
                 </proxies>
                 <activeProfiles><activeProfile>ci</activeProfile></activeProfiles>
               </settings>"#,
        )
        .unwrap();

        assert_eq!(settings.local_repository.as_deref(), Some("/opt/m2"));
        assert!(settings.is_offline());
        assert_eq!(
            settings.server("nexus").unwrap().username.as_deref(),
            Some("ci")
        );
        assert!(settings.server("missing").is_none());
        assert_eq!(settings.mirrors.len(), 1);
        // A proxy with no <active> is active.
        assert!(settings.proxies[0].is_active());
        assert_eq!(settings.proxies[0].port.as_deref(), Some("3128"));
        assert_eq!(settings.active_profiles, vec!["ci"]);
    }

    #[test]
    fn empty_settings_is_all_defaults() {
        let settings = parse_settings("<settings/>").unwrap();
        assert!(!settings.is_offline());
        assert!(settings.mirrors.is_empty());
        assert_eq!(settings, Settings::default());
    }

    fn mirror_of(pattern: &str) -> Mirror {
        Mirror {
            id: Some("m".to_owned()),
            mirror_of: Some(pattern.to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn mirror_wildcard_matches_everything() {
        let mirror = mirror_of("*");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(mirror.matches("anything", "file:/tmp/repo"));
    }

    #[test]
    fn mirror_exact_id() {
        let mirror = mirror_of("central");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(!mirror.matches("jcenter", "https://jcenter.bintray.com"));
    }

    #[test]
    fn mirror_negation_vetoes() {
        let mirror = mirror_of("*,!internal");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(!mirror.matches("internal", "https://nexus.corp/internal"));

        // Order does not matter: an exclusion anywhere wins.
        let reordered = mirror_of("!internal,*");
        assert!(!reordered.matches("internal", "https://nexus.corp/internal"));
        assert!(reordered.matches("central", "https://repo1.maven.org/maven2"));
    }

    #[test]
    fn mirror_multiple_ids() {
        let mirror = mirror_of("central,jcenter");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(mirror.matches("jcenter", "https://jcenter.bintray.com"));
        assert!(!mirror.matches("other", "https://example.com"));
    }

    #[test]
    fn mirror_external_excludes_local() {
        let mirror = mirror_of("external:*");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(!mirror.matches("local", "file:/home/me/repo"));
        assert!(!mirror.matches("dev", "http://localhost:8081/nexus"));
        assert!(!mirror.matches("dev", "http://127.0.0.1:8081/nexus"));
        // A host that merely starts with the loopback name is still external.
        assert!(mirror.matches("dev", "http://localhost.corp/nexus"));
    }

    #[test]
    fn mirror_external_http_only() {
        let mirror = mirror_of("external:http:*");
        assert!(mirror.matches("insecure", "http://repo.example.com/maven2"));
        assert!(!mirror.matches("secure", "https://repo.example.com/maven2"));
        assert!(!mirror.matches("local", "http://localhost/repo"));
    }

    #[test]
    fn mirror_entries_are_not_trimmed() {
        // `central, jcenter` has a second entry of `" jcenter"`, which is not
        // any repository's id. Trimming it reads like a kindness and silently
        // redirects a repository Maven would have left alone.
        let mirror = mirror_of("central, jcenter");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(!mirror.matches("jcenter", "https://jcenter.bintray.com"));
    }

    #[test]
    fn negation_excludes_an_id_and_not_a_wildcard() {
        // `!external:*` excludes a repository literally *named* `external:*`.
        // It is not a negated wildcard, so this mirrors everything — localhost
        // included, which is the surprising half.
        let mirror = mirror_of("*,!external:*");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(mirror.matches("local", "http://localhost/x"));

        // Negating a real id does work.
        let mirror = mirror_of("*,!internal");
        assert!(mirror.matches("central", "https://repo1.maven.org/maven2"));
        assert!(!mirror.matches("internal", "https://internal.corp/repo"));
    }

    #[test]
    fn an_exact_match_stops_the_scan_but_a_wildcard_does_not() {
        // An exact match returns immediately, so a later negation of the same id
        // never runs.
        assert!(mirror_of("central,!central").matches("central", "https://repo1.maven.org/maven2"));
        // A wildcard keeps scanning, so a later negation can still veto it.
        assert!(!mirror_of("*,!central").matches("central", "https://repo1.maven.org/maven2"));
    }

    #[test]
    fn a_lone_pattern_matches_before_the_list_rules_apply() {
        // Upstream short-circuits on the whole pattern, which is why an id
        // containing a comma still matches itself.
        assert!(mirror_of("*").matches("anything", "https://example/repo"));
        assert!(mirror_of("odd,id").matches("odd,id", "https://example/repo"));
    }

    #[test]
    fn webdav_over_http_counts_as_insecure() {
        // WebDAV over HTTP is spelled three ways in the wild and every one is as
        // insecure as plain http, which is what the wildcard is for.
        let mirror = mirror_of("external:http:*");
        for url in [
            "http://repo.example.com/m2",
            "dav:http://repo.example.com/m2",
            "dav+http://repo.example.com/m2",
            "dav://repo.example.com/m2",
        ] {
            assert!(mirror.matches("r", url), "{url} should count as insecure");
        }
        assert!(!mirror.matches("r", "https://repo.example.com/m2"));
        assert!(!mirror.matches("r", "dav:https://repo.example.com/m2"));
    }

    #[test]
    fn mirror_without_mirror_of_matches_nothing() {
        let mirror = Mirror {
            id: Some("m".to_owned()),
            ..Default::default()
        };
        assert!(!mirror.matches("central", "https://repo1.maven.org/maven2"));
    }

    #[test]
    fn encrypted_passwords_are_recognized() {
        let mut server = Server::default();
        assert!(!server.has_encrypted_password());
        server.password = Some("plaintext".to_owned());
        assert!(!server.has_encrypted_password());
        server.password = Some("{jSMOWnoPFgsHVpMvz5VrIt5kRbzGpI8u+9EF1iFQyJQ=}".to_owned());
        assert!(server.has_encrypted_password());
    }

    #[test]
    fn settings_profiles_carry_repositories() {
        let settings = parse_settings(
            r#"<settings><profiles><profile>
                 <id>corp</id>
                 <activation><activeByDefault>true</activeByDefault></activation>
                 <properties><corp.flag>on</corp.flag></properties>
                 <repositories><repository>
                   <id>corp</id><url>https://nexus.corp/public</url>
                 </repository></repositories>
               </profile></profiles></settings>"#,
        )
        .unwrap();
        let profile = &settings.profiles[0];
        assert_eq!(profile.id.as_deref(), Some("corp"));
        assert_eq!(
            profile.activation.as_ref().unwrap().active_by_default,
            Some(true)
        );
        assert_eq!(profile.properties.get("corp.flag").unwrap(), "on");
        assert_eq!(profile.repositories[0].id.as_deref(), Some("corp"));
    }

    #[test]
    fn wrong_root_is_rejected() {
        assert!(parse_settings("<project/>").is_err());
    }
}
