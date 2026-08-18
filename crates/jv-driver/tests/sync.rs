//! What `jv sync` leaves on disk.
//!
//! The companion test `sync_offline_maven.rs` asks real Maven whether the result
//! is usable, which is the answer that matters but needs a JDK, a Maven, and a
//! network. This one checks the shape of what jv writes — layout, tracking
//! files, what is skipped — against a `file:` repository, so it runs everywhere
//! and fails with a specific complaint rather than a Maven build log.

use std::path::{Path, PathBuf};

use jv_driver::{Config, Session, SyncRequest, sync};
use jv_repo::Repository;

/// A repository laid out the way Maven's is.
struct FakeRepository {
    root: PathBuf,
}

impl FakeRepository {
    /// Deploys a snapshot the way a repository serves one: timestamped file
    /// names, with the metadata that says which timestamp is current.
    fn snapshot(&self, group_id: &str, artifact_id: &str, base: &str, resolved: &str) {
        let directory = self
            .root
            .join(group_id.replace('.', "/"))
            .join(artifact_id)
            .join(base);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("{artifact_id}-{resolved}.pom")),
            format!(
                "<project><modelVersion>4.0.0</modelVersion><groupId>{group_id}</groupId>\
                 <artifactId>{artifact_id}</artifactId><version>{base}</version></project>"
            ),
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("{artifact_id}-{resolved}.jar")),
            b"snapshot jar",
        )
        .unwrap();
        let stamp = resolved.rsplit_once('-').unwrap();
        std::fs::write(
            directory.join("maven-metadata.xml"),
            format!(
                r#"<metadata modelVersion="1.1.0"><groupId>{group_id}</groupId>
                     <artifactId>{artifact_id}</artifactId><version>{base}</version>
                     <versioning><snapshot><timestamp>{}</timestamp>
                       <buildNumber>{}</buildNumber></snapshot>
                     <snapshotVersions>
                       <snapshotVersion><extension>pom</extension><value>{resolved}</value></snapshotVersion>
                       <snapshotVersion><extension>jar</extension><value>{resolved}</value></snapshotVersion>
                     </snapshotVersions></versioning></metadata>"#,
                stamp.0.rsplit_once('-').map(|(_, t)| t).unwrap_or(stamp.0),
                stamp.1
            ),
        )
        .unwrap();
    }
}

impl FakeRepository {
    /// Writes a POM and a jar at some coordinates.
    fn artifact(&self, group_id: &str, artifact_id: &str, version: &str, body: &str) -> &Self {
        let directory = self
            .root
            .join(group_id.replace('.', "/"))
            .join(artifact_id)
            .join(version);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("{artifact_id}-{version}.pom")),
            format!(
                "<project><modelVersion>4.0.0</modelVersion>\
                   <groupId>{group_id}</groupId><artifactId>{artifact_id}</artifactId>\
                   <version>{version}</version>{body}</project>"
            ),
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("{artifact_id}-{version}.jar")),
            format!("jar of {artifact_id}"),
        )
        .unwrap();
        self
    }
}

struct Fixture {
    _workspace: tempfile::TempDir,
    session: Session,
    project_pom: PathBuf,
    local_repository: PathBuf,
}

fn fixture(pom_body: &str, populate: impl FnOnce(&FakeRepository)) -> Fixture {
    let workspace = tempfile::tempdir().unwrap();
    let repository_root = workspace.path().join("repo");
    std::fs::create_dir_all(&repository_root).unwrap();
    populate(&FakeRepository {
        root: repository_root.clone(),
    });

    let project = workspace.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_pom = project.join("pom.xml");
    std::fs::write(
        &project_pom,
        format!(
            "<project><modelVersion>4.0.0</modelVersion>\
               <groupId>com.example</groupId><artifactId>app</artifactId><version>1.0</version>\
               {pom_body}</project>"
        ),
    )
    .unwrap();

    let settings = workspace.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();

    let config = Config {
        cache: Some(workspace.path().join("jv-cache")),
        user_settings: Some(settings),
        repositories: Some(vec![Repository::new(
            "local-test",
            format!("file://{}", repository_root.display()),
        )]),
        ..Config::new().without_local_repository()
    };

    Fixture {
        session: Session::new(&config).expect("a session"),
        project_pom,
        local_repository: workspace.path().join("m2"),
        _workspace: workspace,
    }
}

impl Fixture {
    fn sync(&self) -> jv_driver::SyncReport {
        let project = self
            .session
            .project_at(&self.project_pom)
            .expect("a project");
        sync(
            &self.session,
            &project.reactor(),
            &SyncRequest {
                local_repository: Some(self.local_repository.clone()),
                // Plugins reach the network in these fixtures; the dependency
                // half is what this file is about.
                plugins: false,
                plugin_dependencies: false,
                ..SyncRequest::default()
            },
        )
        .expect("a sync")
    }

    fn local(&self, relative: &str) -> PathBuf {
        self.local_repository.join(relative)
    }
}

fn dependency(group_id: &str, artifact_id: &str, version: &str) -> String {
    format!(
        "<dependency><groupId>{group_id}</groupId>\
           <artifactId>{artifact_id}</artifactId><version>{version}</version></dependency>"
    )
}

fn tracking(directory: &Path) -> String {
    std::fs::read_to_string(directory.join("_remote.repositories"))
        .unwrap_or_else(|error| panic!("no tracking file in {}: {error}", directory.display()))
}

#[test]
fn dependencies_land_at_mavens_layout_with_their_poms() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository
                .artifact(
                    "org.test",
                    "a",
                    "1.0",
                    &format!(
                        "<dependencies>{}</dependencies>",
                        dependency("org.test", "b", "1.0")
                    ),
                )
                .artifact("org.test", "b", "1.0", "");
        },
    );

    let report = fixture.sync();
    assert!(report.missing.is_empty(), "missing: {:?}", report.missing);

    for relative in [
        "org/test/a/1.0/a-1.0.jar",
        "org/test/a/1.0/a-1.0.pom",
        // Transitively, too: a build needs the whole tree, not the first level.
        "org/test/b/1.0/b-1.0.jar",
        "org/test/b/1.0/b-1.0.pom",
    ] {
        assert!(
            fixture.local(relative).is_file(),
            "{relative} is not in the local repository"
        );
    }
}

#[test]
fn every_placed_file_is_tracked_as_locally_installed() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository.artifact("org.test", "a", "1.0", "");
        },
    );
    fixture.sync();

    let written = tracking(&fixture.local("org/test/a/1.0"));
    // The locally-installed form is what Maven accepts unconditionally, before
    // it looks at which repositories the build has configured — which is the
    // only form that survives a user with a mirror.
    assert!(written.contains("a-1.0.jar>="), "got:\n{written}");
    assert!(written.contains("a-1.0.pom>="), "got:\n{written}");
    // The real repository is recorded too, because it is true.
    assert!(written.contains("a-1.0.jar>local-test="), "got:\n{written}");
}

#[test]
fn no_placed_file_is_left_mentioned_but_unmatched() {
    // The failure mode this guards: a file mentioned in the tracking file under
    // some id, but not under one the build has configured, is *rejected*
    // offline, and the untracked-file escape hatch does not fire. Every name jv
    // mentions must therefore also carry the unconditional form.
    let fixture = fixture(
        &format!(
            "<dependencies>{}{}</dependencies>",
            dependency("org.test", "a", "1.0"),
            dependency("org.test", "b", "1.0")
        ),
        |repository| {
            repository
                .artifact("org.test", "a", "1.0", "")
                .artifact("org.test", "b", "1.0", "");
        },
    );
    fixture.sync();

    for directory in ["org/test/a/1.0", "org/test/b/1.0"] {
        let written = tracking(&fixture.local(directory));
        let mentioned: std::collections::BTreeSet<&str> = written
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split('>').next())
            .collect();
        for name in mentioned {
            assert!(
                written.contains(&format!("{name}>=")),
                "{directory}: {name} is mentioned without the unconditional form:\n{written}"
            );
        }
    }
}

#[test]
fn a_file_maven_already_placed_is_left_alone() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository.artifact("org.test", "a", "1.0", "");
        },
    );

    let existing = fixture.local("org/test/a/1.0/a-1.0.jar");
    std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
    std::fs::write(&existing, b"maven put this here").unwrap();

    fixture.sync();
    // That directory belongs to Maven; a file already in it is Maven's answer,
    // not jv's to correct.
    assert_eq!(std::fs::read(&existing).unwrap(), b"maven put this here");
}

#[test]
fn tracking_entries_maven_wrote_survive_a_sync() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository.artifact("org.test", "a", "1.0", "");
        },
    );

    let directory = fixture.local("org/test/a/1.0");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("_remote.repositories"),
        "#NOTE: whatever\na-1.0-sources.jar>central=\n",
    )
    .unwrap();

    fixture.sync();
    let written = tracking(&directory);
    // Dropping Maven's entry would leave the file it downloaded mentioned
    // nowhere — and that is a file jv did not place and cannot replace.
    assert!(
        written.contains("a-1.0-sources.jar>central="),
        "got:\n{written}"
    );
    assert!(written.contains("a-1.0.jar>="), "got:\n{written}");
}

#[test]
fn the_reactors_own_modules_are_not_looked_for() {
    let workspace = tempfile::tempdir().unwrap();
    let repository_root = workspace.path().join("repo");
    std::fs::create_dir_all(&repository_root).unwrap();

    let project = workspace.path().join("project");
    std::fs::create_dir_all(project.join("lib")).unwrap();
    std::fs::write(
        project.join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion>\
           <groupId>com.example</groupId><artifactId>root</artifactId><version>1.0</version>\
           <packaging>pom</packaging><modules><module>lib</module></modules></project>",
    )
    .unwrap();
    std::fs::write(
        project.join("lib/pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion>\
           <parent><groupId>com.example</groupId><artifactId>root</artifactId>\
             <version>1.0</version></parent><artifactId>lib</artifactId></project>",
    )
    .unwrap();

    let settings = workspace.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();
    let config = Config {
        cache: Some(workspace.path().join("jv-cache")),
        user_settings: Some(settings),
        repositories: Some(vec![Repository::new(
            "local-test",
            format!("file://{}", repository_root.display()),
        )]),
        ..Config::new().without_local_repository()
    };
    let session = Session::new(&config).expect("a session");
    let root = session
        .project_at(&project.join("pom.xml"))
        .expect("a project");

    let report = sync(
        &session,
        &root.reactor(),
        &SyncRequest {
            local_repository: Some(workspace.path().join("m2")),
            plugins: false,
            plugin_dependencies: false,
            ..SyncRequest::default()
        },
    )
    .expect("a sync");

    // Nothing has published `com.example:lib`; asking a repository for it
    // produces a 404 that means nothing and a report entry that alarms for no
    // reason.
    assert!(
        !report.missing.iter().any(|missing| missing.contains("lib")),
        "the reactor's own module was looked for: {:?}",
        report.missing
    );
}

#[test]
fn a_snapshot_is_placed_as_a_locally_installed_one() {
    // A downloaded snapshot's file name carries a deployment timestamp, and
    // Maven only learns which timestamp is current from metadata whose *file
    // name* carries the effective repository id — which jv cannot know the next
    // `mvn` will be configured with. So jv writes the shape `mvn install`
    // produces instead: base-version file names and a `maven-metadata-local.xml`
    // declaring a local copy, which Maven accepts from any configuration.
    let fixture = fixture(
        "<dependencies><dependency><groupId>org.test</groupId>\
           <artifactId>snap</artifactId><version>1.0-SNAPSHOT</version></dependency></dependencies>",
        |repository| {
            repository.snapshot("org.test", "snap", "1.0-SNAPSHOT", "1.0-20240115.103000-7");
        },
    );
    let report = fixture.sync();
    assert!(report.missing.is_empty(), "missing: {:?}", report.missing);

    let directory = fixture.local("org/test/snap/1.0-SNAPSHOT");
    // The base name, not the timestamped one.
    assert!(directory.join("snap-1.0-SNAPSHOT.jar").is_file());
    assert!(directory.join("snap-1.0-SNAPSHOT.pom").is_file());
    assert!(!directory.join("snap-1.0-20240115.103000-7.jar").exists());

    let metadata = std::fs::read_to_string(directory.join("maven-metadata-local.xml"))
        .expect("snapshot metadata");
    // `localCopy` is the part that makes this resolvable without a repository id
    // anywhere in the file to get wrong.
    assert!(metadata.contains("<localCopy>true</localCopy>"));
    assert!(metadata.contains("<version>1.0-SNAPSHOT</version>"));
    assert!(metadata.contains("<extension>jar</extension>"));
    assert!(metadata.contains("<extension>pom</extension>"));
    assert!(!metadata.contains("20240115.103000-7"));
}
