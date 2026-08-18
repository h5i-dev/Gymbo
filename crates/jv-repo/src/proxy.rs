//! Which proxy, if any, a request to a repository goes through.
//!
//! Port of maven-resolver's `DefaultProxySelector`, which is what Maven builds
//! from `settings.xml` `<proxies>`. The selection is not "the first active
//! proxy": it is per-protocol, with the repository's host checked against that
//! proxy's own `<nonProxyHosts>` *before* the protocol is looked at.
//!
//! Three details are easy to get wrong and are all deliberate here:
//!
//! * The first proxy of a given type wins. Later ones of the same type are
//!   ignored, so `<proxies>` order is significant.
//! * An `https` repository falls back to an `http` proxy when no `https` proxy
//!   matched. The reverse is not true.
//! * `nonProxyHosts` is matched per proxy, not globally — a host excluded from
//!   one proxy can still be routed through another.

use std::collections::HashMap;

use jv_model::Settings;

/// A proxy, resolved to the form a client needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyEndpoint {
    /// `http://[user:password@]host[:port]`, ready to hand to a transport.
    pub url: String,
    /// The `<id>`, for diagnostics.
    pub id: Option<String>,
}

/// The `<proxies>` of a `settings.xml`, ready to answer "which proxy for this
/// URL".
#[derive(Clone, Debug, Default)]
pub struct ProxySelector {
    entries: Vec<Entry>,
}

#[derive(Clone, Debug)]
struct Entry {
    /// Lower-cased. `<protocol>` defaults to `http`, as Maven's settings model
    /// does.
    protocol: String,
    endpoint: ProxyEndpoint,
    non_proxy_hosts: NonProxyHosts,
}

impl ProxySelector {
    /// Builds a selector from a settings file, keeping only active proxies.
    pub fn from_settings(settings: &Settings) -> Self {
        let entries = settings
            .proxies
            .iter()
            .filter(|proxy| proxy.is_active())
            .filter_map(|proxy| {
                let host = proxy.host.as_deref()?.trim();
                if host.is_empty() {
                    return None;
                }
                // Maven's settings model defaults `<protocol>` to http.
                let protocol = proxy
                    .protocol
                    .as_deref()
                    .filter(|protocol| !protocol.trim().is_empty())
                    .unwrap_or("http")
                    .trim()
                    .to_ascii_lowercase();

                let mut url = String::from("http://");
                if let Some(username) = proxy.username.as_deref().filter(|u| !u.is_empty()) {
                    url.push_str(&encode_userinfo(username));
                    if let Some(password) = proxy.password.as_deref() {
                        url.push(':');
                        url.push_str(&encode_userinfo(password));
                    }
                    url.push('@');
                }
                url.push_str(host);
                if let Some(port) = proxy.port.as_deref().filter(|port| !port.trim().is_empty()) {
                    url.push(':');
                    url.push_str(port.trim());
                }

                Some(Entry {
                    protocol,
                    endpoint: ProxyEndpoint {
                        url,
                        id: proxy.id.clone(),
                    },
                    non_proxy_hosts: NonProxyHosts::new(proxy.non_proxy_hosts.as_deref()),
                })
            })
            .collect();
        Self { entries }
    }

    /// Whether any active proxy is configured at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The proxy for a repository URL, or `None` to connect directly.
    pub fn select(&self, url: &str) -> Option<&ProxyEndpoint> {
        let (protocol, host) = split_url(url)?;
        self.select_parts(&protocol, &host)
    }

    /// The selection itself, on already-split parts.
    ///
    /// Upstream builds the candidate map first — excluding, per proxy, any
    /// whose `nonProxyHosts` covers this host — and only then looks up by
    /// protocol. Doing it in the other order would let an excluded proxy shadow
    /// a usable one of the same type.
    fn select_parts(&self, protocol: &str, host: &str) -> Option<&ProxyEndpoint> {
        let mut candidates: HashMap<&str, &Entry> = HashMap::new();
        for entry in &self.entries {
            if entry.non_proxy_hosts.covers(host) {
                continue;
            }
            candidates.entry(entry.protocol.as_str()).or_insert(entry);
        }

        let protocol = normalize_protocol(protocol);
        candidates
            .get(protocol.as_str())
            // An https repository accepts an http proxy; not the other way round.
            .or_else(|| {
                if protocol == "https" {
                    candidates.get("http")
                } else {
                    None
                }
            })
            .map(|entry| &entry.endpoint)
    }
}

/// `davs` → https, `dav` → http, `dav:https` → https.
fn normalize_protocol(protocol: &str) -> String {
    let protocol = protocol.to_ascii_lowercase();
    match protocol.as_str() {
        "davs" => "https".to_owned(),
        "dav" => "http".to_owned(),
        _ => match protocol.strip_prefix("dav:") {
            Some(rest) => rest.to_owned(),
            None => protocol,
        },
    }
}

/// Splits `https://host:443/path` into (`https`, `host`).
fn split_url(url: &str) -> Option<(String, String)> {
    let (protocol, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip any userinfo, then the port.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.strip_prefix('[') {
        // IPv6 literal: the colons inside the brackets are not a port.
        Some(inner) => inner.split(']').next().unwrap_or(inner),
        None => host.split(':').next().unwrap_or(host),
    };
    if host.is_empty() {
        return None;
    }
    Some((protocol.to_owned(), host.to_owned()))
}

/// Percent-encodes the characters that would otherwise end the userinfo.
fn encode_userinfo(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// A `|`-separated list of host patterns, where `*` is a wildcard.
#[derive(Clone, Debug, Default)]
struct NonProxyHosts {
    patterns: Vec<String>,
}

impl NonProxyHosts {
    fn new(spec: Option<&str>) -> Self {
        let patterns = spec
            .unwrap_or_default()
            .split('|')
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .map(str::to_owned)
            .collect();
        Self { patterns }
    }

    /// Whether this host bypasses the proxy.
    ///
    /// Upstream compiles each pattern by escaping `.` and turning `*` into
    /// `.*`, then requires a *full* match, case-insensitively. Matching the
    /// whole string is what makes `eclipse.org` fail to cover
    /// `www.eclipse.org`.
    fn covers(&self, host: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| glob_matches(pattern, host))
    }
}

/// A full-string, case-insensitive glob where `*` matches any run of
/// characters and every other character is literal.
fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let value: Vec<char> = value.to_lowercase().chars().collect();

    // Classic two-cursor wildcard match: linear, and no regex dependency for
    // what is a two-metacharacter language.
    let (mut p, mut v) = (0usize, 0usize);
    let (mut star, mut retry) = (None, 0usize);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            retry = v;
            p += 1;
        } else if let Some(last_star) = star {
            p = last_star + 1;
            retry += 1;
            v = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jv_model::Proxy;

    fn non_proxy(host: &str, spec: &str) -> bool {
        NonProxyHosts::new(Some(spec)).covers(host)
    }

    // The five cases below are ported from upstream's own
    // `DefaultProxySelectorTest`, so a divergence in the matcher shows up as a
    // failure here rather than as a request that silently bypassed a proxy.

    #[test]
    fn a_blank_non_proxy_list_covers_nothing() {
        assert!(!NonProxyHosts::new(None).covers("www.eclipse.org"));
        assert!(!non_proxy("www.eclipse.org", ""));
    }

    #[test]
    fn wildcards_match_the_way_upstream_compiles_them() {
        assert!(non_proxy("www.eclipse.org", "*"));
        assert!(non_proxy("www.eclipse.org", "*.org"));
        assert!(!non_proxy("www.eclipse.org", "*.com"));
        assert!(non_proxy("www.eclipse.org", "www.*"));
        assert!(non_proxy("www.eclipse.org", "www.*.org"));
    }

    #[test]
    fn any_entry_in_the_list_is_enough() {
        assert!(non_proxy("eclipse.org", "eclipse.org|host2"));
        assert!(non_proxy("eclipse.org", "host1|eclipse.org"));
        assert!(non_proxy("eclipse.org", "host1|eclipse.org|host2"));
    }

    #[test]
    fn a_pattern_must_match_the_whole_host() {
        assert!(!non_proxy("www.eclipse.org", "www.eclipse.com"));
        // The one that bites people: a bare domain does not cover subdomains.
        assert!(!non_proxy("www.eclipse.org", "eclipse.org"));
    }

    #[test]
    fn matching_ignores_case_on_both_sides() {
        assert!(non_proxy("www.eclipse.org", "www.ECLIPSE.org"));
        assert!(non_proxy("www.ECLIPSE.org", "www.eclipse.org"));
    }

    fn proxy(id: &str, protocol: Option<&str>, host: &str, non_proxy: Option<&str>) -> Proxy {
        Proxy {
            id: Some(id.to_owned()),
            active: None,
            protocol: protocol.map(str::to_owned),
            host: Some(host.to_owned()),
            port: Some("8080".to_owned()),
            username: None,
            password: None,
            non_proxy_hosts: non_proxy.map(str::to_owned),
        }
    }

    fn selector(proxies: Vec<Proxy>) -> ProxySelector {
        ProxySelector::from_settings(&Settings {
            proxies,
            ..Settings::default()
        })
    }

    #[test]
    fn a_proxy_is_chosen_by_protocol() {
        let selector = selector(vec![
            proxy("p-http", Some("http"), "http.example", None),
            proxy("p-https", Some("https"), "https.example", None),
        ]);
        assert_eq!(
            selector.select("https://repo.example/a").map(|p| &p.url),
            Some(&"http://https.example:8080".to_owned())
        );
        assert_eq!(
            selector.select("http://repo.example/a").map(|p| &p.url),
            Some(&"http://http.example:8080".to_owned())
        );
    }

    #[test]
    fn https_falls_back_to_an_http_proxy_but_not_the_reverse() {
        let https = selector(vec![proxy("p", Some("http"), "only.example", None)]);
        assert!(https.select("https://repo.example/a").is_some());

        let http = selector(vec![proxy("p", Some("https"), "only.example", None)]);
        assert!(
            http.select("http://repo.example/a").is_none(),
            "an http request must not be routed through an https-only proxy"
        );
    }

    #[test]
    fn the_first_proxy_of_a_type_wins() {
        let selector = selector(vec![
            proxy("first", Some("http"), "first.example", None),
            proxy("second", Some("http"), "second.example", None),
        ]);
        assert_eq!(
            selector.select("http://repo.example/a").map(|p| &p.url),
            Some(&"http://first.example:8080".to_owned())
        );
    }

    #[test]
    fn non_proxy_hosts_are_evaluated_before_the_protocol_lookup() {
        // The excluded proxy is first, so an implementation that picked by
        // protocol and only then checked nonProxyHosts would return nothing
        // here instead of falling through to the second entry.
        let selector = selector(vec![
            proxy(
                "excluded",
                Some("http"),
                "first.example",
                Some("repo.example"),
            ),
            proxy("usable", Some("http"), "second.example", None),
        ]);
        assert_eq!(
            selector.select("http://repo.example/a").map(|p| &p.url),
            Some(&"http://second.example:8080".to_owned()),
            "a proxy excluded for this host must not shadow the next of its type"
        );
    }

    #[test]
    fn an_absent_protocol_means_http() {
        let selector = selector(vec![proxy("p", None, "default.example", None)]);
        assert!(selector.select("http://repo.example/a").is_some());
    }

    #[test]
    fn an_inactive_proxy_is_ignored() {
        let mut inactive = proxy("p", Some("http"), "off.example", None);
        inactive.active = Some(false);
        assert!(selector(vec![inactive]).is_empty());
    }

    #[test]
    fn credentials_are_encoded_into_the_endpoint() {
        let mut authenticated = proxy("p", Some("http"), "corp.example", None);
        authenticated.username = Some("user@corp".to_owned());
        authenticated.password = Some("p@ss:word".to_owned());
        let selector = selector(vec![authenticated]);
        assert_eq!(
            selector.select("http://repo.example/a").map(|p| &p.url),
            // `@` and `:` must be escaped or they would end the userinfo early
            // and the request would go to the wrong host.
            Some(&"http://user%40corp:p%40ss%3Aword@corp.example:8080".to_owned())
        );
    }

    #[test]
    fn dav_protocols_map_onto_http() {
        let selector = selector(vec![proxy("p", Some("https"), "secure.example", None)]);
        assert!(selector.select("davs://repo.example/a").is_some());
        assert!(selector.select("dav:https://repo.example/a").is_some());
    }

    #[test]
    fn the_host_is_taken_without_port_or_userinfo() {
        assert_eq!(
            split_url("https://user:pw@repo.example:8443/path"),
            Some(("https".to_owned(), "repo.example".to_owned()))
        );
        assert_eq!(
            split_url("http://[2001:db8::1]:8080/x"),
            Some(("http".to_owned(), "2001:db8::1".to_owned()))
        );
    }
}
