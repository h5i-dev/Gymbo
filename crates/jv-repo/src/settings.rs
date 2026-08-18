//! Combining the installation's `settings.xml` with the user's.
//!
//! Maven reads two files — `$MAVEN_HOME/conf/settings.xml` and
//! `~/.m2/settings.xml` — and merges them with the user's winning. The merge is
//! *shallow and by id*: a user `<server>` with the same id as an installation one
//! replaces it outright rather than filling in its blanks. That is worth being
//! exact about, because the common corporate setup puts a mirror in the
//! installation file and credentials in the user file, and getting the rule
//! backwards silently drops one of them.
//!
//! Ported from `SettingsUtils.merge` and `shallowMergeById`
//! (`maven-settings-builder`). Finding the files is the caller's job; this is
//! pure.

use jv_model::{Mirror, Proxy, Repository as RepositoryModel, Server, Settings, SettingsProfile};

/// Merges two settings files, `dominant` winning.
///
/// Call it as `merge(user, installation)`: the user's file is the dominant one.
///
/// # Examples
///
/// ```
/// use jv_model::parse_settings;
/// use jv_repo::merge_settings;
///
/// let installation = parse_settings(
///     r#"<settings>
///          <localRepository>/opt/m2</localRepository>
///          <mirrors><mirror><id>nexus</id><url>https://old</url></mirror></mirrors>
///        </settings>"#,
/// ).unwrap();
/// let user = parse_settings(
///     r#"<settings>
///          <mirrors><mirror><id>nexus</id><url>https://new</url></mirror></mirrors>
///        </settings>"#,
/// ).unwrap();
///
/// let merged = merge_settings(user, &installation);
/// // The user's mirror replaces the installation's entirely, by id...
/// assert_eq!(merged.mirrors.len(), 1);
/// assert_eq!(merged.mirrors[0].url.as_deref(), Some("https://new"));
/// // ...while a setting the user did not state falls through.
/// assert_eq!(merged.local_repository.as_deref(), Some("/opt/m2"));
/// ```
pub fn merge_settings(dominant: Settings, recessive: &Settings) -> Settings {
    let mut merged = dominant;

    // Scalars: the user's value stands unless there isn't one.
    if merged.local_repository.is_none() {
        merged.local_repository = recessive.local_repository.clone();
    }
    if merged.interactive_mode.is_none() {
        merged.interactive_mode = recessive.interactive_mode;
    }
    if merged.offline.is_none() {
        merged.offline = recessive.offline;
    }

    merge_by_id(&mut merged.mirrors, &recessive.mirrors, |m: &Mirror| {
        m.id.as_deref()
    });
    merge_by_id(&mut merged.servers, &recessive.servers, |s: &Server| {
        s.id.as_deref()
    });
    merge_by_id(&mut merged.proxies, &recessive.proxies, |p: &Proxy| {
        p.id.as_deref()
    });
    merge_by_id(
        &mut merged.repositories,
        &recessive.repositories,
        |r: &RepositoryModel| r.id.as_deref(),
    );
    merge_by_id(
        &mut merged.plugin_repositories,
        &recessive.plugin_repositories,
        |r: &RepositoryModel| r.id.as_deref(),
    );
    merge_by_id(
        &mut merged.profiles,
        &recessive.profiles,
        |p: &SettingsProfile| p.id.as_deref(),
    );

    // Plain string lists: union, keeping the user's order first.
    merge_strings(&mut merged.active_profiles, &recessive.active_profiles);
    merge_strings(&mut merged.plugin_groups, &recessive.plugin_groups);

    merged
}

/// Appends the recessive entries whose id the dominant list does not already
/// carry.
///
/// An entry with no id is appended unconditionally: there is nothing to collide
/// on, and dropping it would lose configuration the user wrote.
fn merge_by_id<T: Clone>(dominant: &mut Vec<T>, recessive: &[T], id: impl Fn(&T) -> Option<&str>) {
    let taken: Vec<String> = dominant
        .iter()
        .filter_map(|entry| id(entry).map(str::to_owned))
        .collect();
    for entry in recessive {
        match id(entry) {
            Some(entry_id) if taken.iter().any(|held| held == entry_id) => continue,
            _ => dominant.push(entry.clone()),
        }
    }
}

fn merge_strings(dominant: &mut Vec<String>, recessive: &[String]) {
    for value in recessive {
        if !dominant.contains(value) {
            dominant.push(value.clone());
        }
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
    fn merging_with_an_empty_file_changes_nothing() {
        let user = settings(
            r#"<settings><mirrors><mirror><id>a</id><url>u</url></mirror></mirrors></settings>"#,
        );
        assert_eq!(merge_settings(user.clone(), &Settings::default()), user);
        assert_eq!(merge_settings(Settings::default(), &user), user);
    }

    #[test]
    fn a_user_entry_replaces_the_installation_one_wholesale() {
        let installation = settings(
            r#"<settings><servers><server>
                 <id>nexus</id><username>build</username><password>old</password>
               </server></servers></settings>"#,
        );
        let user = settings(
            r#"<settings><servers><server>
                 <id>nexus</id><username>me</username>
               </server></servers></settings>"#,
        );
        let merged = merge_settings(user, &installation);
        assert_eq!(merged.servers.len(), 1);
        assert_eq!(merged.servers[0].username.as_deref(), Some("me"));
        // The merge is shallow: the installation's password does not fill in the
        // blank the user left, which is what Maven does and what a user
        // deliberately clearing a password expects.
        assert_eq!(merged.servers[0].password, None);
    }

    #[test]
    fn unmatched_installation_entries_are_kept_after_the_users() {
        let installation = settings(
            r#"<settings><mirrors>
                 <mirror><id>corp</id><url>https://corp</url></mirror>
                 <mirror><id>shared</id><url>https://shared</url></mirror>
               </mirrors></settings>"#,
        );
        let user = settings(
            r#"<settings><mirrors>
                 <mirror><id>corp</id><url>https://mine</url></mirror>
               </mirrors></settings>"#,
        );
        let merged = merge_settings(user, &installation);
        // Order decides which mirror claims a repository first, so the user's
        // must stay in front.
        let ids: Vec<_> = merged
            .mirrors
            .iter()
            .filter_map(|m| m.id.as_deref())
            .collect();
        assert_eq!(ids, ["corp", "shared"]);
        assert_eq!(merged.mirrors[0].url.as_deref(), Some("https://mine"));
    }

    #[test]
    fn scalars_take_the_users_value_when_it_has_one() {
        let installation = settings(
            r#"<settings><localRepository>/opt/m2</localRepository><offline>true</offline></settings>"#,
        );
        let user = settings(
            r#"<settings><localRepository>/home/me/.m2/repository</localRepository></settings>"#,
        );
        let merged = merge_settings(user, &installation);
        assert_eq!(
            merged.local_repository.as_deref(),
            Some("/home/me/.m2/repository")
        );
        // Not stated by the user, so the installation's stands.
        assert_eq!(merged.offline, Some(true));
    }

    #[test]
    fn active_profiles_are_a_union() {
        let installation = settings(
            r#"<settings><activeProfiles><activeProfile>corp</activeProfile><activeProfile>shared</activeProfile></activeProfiles></settings>"#,
        );
        let user = settings(
            r#"<settings><activeProfiles><activeProfile>corp</activeProfile><activeProfile>mine</activeProfile></activeProfiles></settings>"#,
        );
        let merged = merge_settings(user, &installation);
        assert_eq!(merged.active_profiles, ["corp", "mine", "shared"]);
    }

    #[test]
    fn entries_without_an_id_are_all_kept() {
        // Nothing to collide on, and silently dropping one would lose
        // configuration someone wrote on purpose.
        let installation = settings(
            r#"<settings><mirrors><mirror><url>https://a</url></mirror></mirrors></settings>"#,
        );
        let user = settings(
            r#"<settings><mirrors><mirror><url>https://b</url></mirror></mirrors></settings>"#,
        );
        let merged = merge_settings(user, &installation);
        assert_eq!(merged.mirrors.len(), 2);
    }
}
