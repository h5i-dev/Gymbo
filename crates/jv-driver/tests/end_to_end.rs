//! The whole pipeline, against a repository on disk.
//!
//! Every other test in the workspace exercises one layer. This one runs a real
//! project through settings, repository selection, fetching, caching, effective
//! model building, collection, conflict resolution and rendering — the path a
//! user's `jv tree` takes — using a `file:` repository so it needs no network and
//! no fixtures anyone has to keep in sync.

use std::path::{Path, PathBuf};

use jv_driver::{Config, Session};
use jv_repo::Repository;
use jv_resolver::Verbosity;
use jv_tree::{Format, Options, render};

/// A repository laid out the way Maven's is.
struct FakeRepository {
    root: PathBuf,
}

impl FakeRepository {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Writes a POM at its coordinates.
    fn pom(&self, group_id: &str, artifact_id: &str, version: &str, body: &str) -> &Self {
        let directory = self
            .root
            .join(group_id.replace('.', "/"))
            .join(artifact_id)
            .join(version);
        std::fs::create_dir_all(&directory).unwrap();
        let pom = format!(
            "<project>\
               <modelVersion>4.0.0</modelVersion>\
               <groupId>{group_id}</groupId>\
               <artifactId>{artifact_id}</artifactId>\
               <version>{version}</version>\
               {body}\
             </project>"
        );
        std::fs::write(directory.join(format!("{artifact_id}-{version}.pom")), pom).unwrap();
        self
    }

    fn url(&self) -> String {
        format!("file://{}", self.root.display())
    }

    fn as_repository(&self) -> Repository {
        Repository::new("local-test", self.url())
    }
}

fn dependency(group_id: &str, artifact_id: &str, version: &str) -> String {
    format!(
        "<dependency><groupId>{group_id}</groupId>\
           <artifactId>{artifact_id}</artifactId><version>{version}</version></dependency>"
    )
}

/// A session and project rooted at a temporary directory.
struct Fixture {
    _cache: tempfile::TempDir,
    _repository: tempfile::TempDir,
    project: tempfile::TempDir,
    session: Session,
}

impl Fixture {
    fn tree(&self, verbosity: Verbosity, options: Options) -> String {
        let project = self
            .session
            .project(self.project.path())
            .expect("a project");
        let resolution = self
            .session
            .resolve_project(&project, verbosity)
            .expect("a resolution");
        render(&resolution.collected.graph, Format::Text, options)
    }
}

/// Builds a project whose `pom.xml` is `body`, against a repository populated by
/// `populate`.
fn fixture(body: &str, populate: impl FnOnce(&FakeRepository)) -> Fixture {
    let repository_dir = tempfile::tempdir().unwrap();
    let repository = FakeRepository::new(repository_dir.path());
    populate(&repository);

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("pom.xml"),
        format!(
            "<project>\
               <modelVersion>4.0.0</modelVersion>\
               <groupId>com.example</groupId>\
               <artifactId>app</artifactId>\
               <version>1.0</version>\
               {body}\
             </project>"
        ),
    )
    .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache: Some(cache.path().to_path_buf()),
        repositories: Some(vec![repository.as_repository()]),
        // A populated ~/.m2 or a real settings.xml on the developer's machine
        // would otherwise decide what this test resolves.
        user_settings: Some(project.path().join("settings.xml")),
        ..Config::new().without_local_repository()
    };
    std::fs::write(project.path().join("settings.xml"), "<settings/>").unwrap();

    Fixture {
        session: Session::new(&config).expect("a session"),
        _cache: cache,
        _repository: repository_dir,
        project,
    }
}

#[test]
fn a_transitive_dependency_appears_under_its_parent() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository
                .pom(
                    "org.test",
                    "a",
                    "1.0",
                    &format!(
                        "<dependencies>{}</dependencies>",
                        dependency("org.test", "b", "1.0")
                    ),
                )
                .pom("org.test", "b", "1.0", "");
        },
    );

    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\
         \\- org.test:a:jar:1.0:compile\n\
         \x20  \\- org.test:b:jar:1.0:compile\n"
    );
}

#[test]
fn the_nearest_version_wins_and_the_loser_disappears() {
    // `b:2.0` is declared directly and so is nearer than the `b:1.0` that `a`
    // pulls in. Maven keeps the nearer one and drops the other entirely.
    let fixture = fixture(
        &format!(
            "<dependencies>{}{}</dependencies>",
            dependency("org.test", "a", "1.0"),
            dependency("org.test", "b", "2.0")
        ),
        |repository| {
            repository
                .pom(
                    "org.test",
                    "a",
                    "1.0",
                    &format!(
                        "<dependencies>{}</dependencies>",
                        dependency("org.test", "b", "1.0")
                    ),
                )
                .pom("org.test", "b", "1.0", "")
                .pom("org.test", "b", "2.0", "");
        },
    );

    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\
         +- org.test:a:jar:1.0:compile\n\
         \\- org.test:b:jar:2.0:compile\n"
    );
}

#[test]
fn verbose_output_names_the_version_that_won() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}{}</dependencies>",
            dependency("org.test", "a", "1.0"),
            dependency("org.test", "b", "2.0")
        ),
        |repository| {
            repository
                .pom(
                    "org.test",
                    "a",
                    "1.0",
                    &format!(
                        "<dependencies>{}</dependencies>",
                        dependency("org.test", "b", "1.0")
                    ),
                )
                .pom("org.test", "b", "1.0", "")
                .pom("org.test", "b", "2.0", "");
        },
    );

    let tree = fixture.tree(
        Verbosity::Full,
        Options {
            verbose: true,
            ..Options::default()
        },
    );
    // The point of verbose mode is being told *why* a version is not there.
    assert!(
        tree.contains("omitted for conflict with 2.0"),
        "expected a conflict annotation, got:\n{tree}"
    );
}

#[test]
fn dependency_management_supplies_a_version_the_declaration_omits() {
    let fixture = fixture(
        &format!(
            "<dependencyManagement><dependencies>{}</dependencies></dependencyManagement>\
             <dependencies>\
               <dependency><groupId>org.test</groupId><artifactId>a</artifactId></dependency>\
             </dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository.pom("org.test", "a", "1.0", "");
        },
    );

    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\\- org.test:a:jar:1.0:compile\n"
    );
}

#[test]
fn a_bom_import_supplies_versions() {
    let fixture = fixture(
        "<dependencyManagement><dependencies>\
           <dependency><groupId>org.test</groupId><artifactId>bom</artifactId>\
             <version>1.0</version><type>pom</type><scope>import</scope></dependency>\
         </dependencies></dependencyManagement>\
         <dependencies>\
           <dependency><groupId>org.test</groupId><artifactId>a</artifactId></dependency>\
         </dependencies>",
        |repository| {
            repository
                .pom(
                    "org.test",
                    "bom",
                    "1.0",
                    &format!(
                        "<packaging>pom</packaging>\
                         <dependencyManagement><dependencies>{}</dependencies></dependencyManagement>",
                        dependency("org.test", "a", "3.1")
                    ),
                )
                .pom("org.test", "a", "3.1", "");
        },
    );

    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\\- org.test:a:jar:3.1:compile\n"
    );
}

#[test]
fn an_exclusion_prunes_a_subtree() {
    let fixture = fixture(
        "<dependencies><dependency>\
           <groupId>org.test</groupId><artifactId>a</artifactId><version>1.0</version>\
           <exclusions><exclusion>\
             <groupId>org.test</groupId><artifactId>b</artifactId>\
           </exclusion></exclusions>\
         </dependency></dependencies>",
        |repository| {
            repository
                .pom(
                    "org.test",
                    "a",
                    "1.0",
                    &format!(
                        "<dependencies>{}</dependencies>",
                        dependency("org.test", "b", "1.0")
                    ),
                )
                .pom(
                    "org.test",
                    "b",
                    "1.0",
                    &format!(
                        "<dependencies>{}</dependencies>",
                        dependency("org.test", "c", "1.0")
                    ),
                )
                .pom("org.test", "c", "1.0", "");
        },
    );

    // Excluding `b` must take `c` with it: an exclusion cuts the edge, and
    // everything only reachable through it goes too.
    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\\- org.test:a:jar:1.0:compile\n"
    );
}

#[test]
fn a_test_dependency_of_a_dependency_stays_out() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository
                .pom(
                    "org.test",
                    "a",
                    "1.0",
                    "<dependencies><dependency>\
                       <groupId>org.test</groupId><artifactId>junit</artifactId>\
                       <version>1.0</version><scope>test</scope>\
                     </dependency></dependencies>",
                )
                .pom("org.test", "junit", "1.0", "");
        },
    );

    // The project's own test dependencies are in the graph; a dependency's are
    // never anyone else's problem.
    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\\- org.test:a:jar:1.0:compile\n"
    );
}

#[test]
fn the_projects_own_test_dependency_is_kept() {
    let fixture = fixture(
        "<dependencies><dependency>\
           <groupId>org.test</groupId><artifactId>junit</artifactId>\
           <version>1.0</version><scope>test</scope>\
         </dependency></dependencies>",
        |repository| {
            repository.pom("org.test", "junit", "1.0", "");
        },
    );

    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\\- org.test:junit:jar:1.0:test\n"
    );
}

#[test]
fn a_property_from_a_parent_resolves_a_dependency_version() {
    let fixture = fixture(
        "<parent><groupId>org.test</groupId><artifactId>parent</artifactId>\
           <version>1.0</version></parent>\
         <dependencies><dependency>\
           <groupId>org.test</groupId><artifactId>a</artifactId>\
           <version>${a.version}</version>\
         </dependency></dependencies>",
        |repository| {
            repository
                .pom(
                    "org.test",
                    "parent",
                    "1.0",
                    "<packaging>pom</packaging>\
                     <properties><a.version>2.5</a.version></properties>",
                )
                .pom("org.test", "a", "2.5", "");
        },
    );

    // The parent came from the repository, its property survived inheritance,
    // and interpolation used it — three stages that only meet here.
    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\\- org.test:a:jar:2.5:compile\n"
    );
}

#[test]
fn a_missing_pom_leaves_a_childless_node_rather_than_failing() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "absent", "1.0")
        ),
        |_| {},
    );

    // Maven carries on past a POM it cannot read; the failure belongs to the
    // point where the jar is actually needed.
    assert_eq!(
        fixture.tree(Verbosity::None, Options::default()),
        "com.example:app:jar:1.0\n\\- org.test:absent:jar:1.0:compile\n"
    );
}

#[test]
fn a_relocated_artifact_resolves_to_its_new_coordinates() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "old", "1.0")
        ),
        |repository| {
            repository
                .pom(
                    "org.test",
                    "old",
                    "1.0",
                    "<distributionManagement><relocation>\
                       <artifactId>new</artifactId>\
                       <message>renamed in 1.0</message>\
                     </relocation></distributionManagement>",
                )
                .pom(
                    "org.test",
                    "new",
                    "1.0",
                    &format!(
                        "<dependencies>{}</dependencies>",
                        dependency("org.test", "b", "1.0")
                    ),
                )
                .pom("org.test", "b", "1.0", "");
        },
    );

    // The relocated artifact's *own* dependencies are what enter the graph.
    let tree = fixture.tree(Verbosity::None, Options::default());
    assert!(
        tree.contains("org.test:b:jar:1.0"),
        "the relocation target's dependencies should be collected, got:\n{tree}"
    );
}

#[test]
fn a_second_resolve_is_served_from_the_cache() {
    let fixture = fixture(
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "a", "1.0")
        ),
        |repository| {
            repository.pom("org.test", "a", "1.0", "");
        },
    );

    let first = fixture.tree(Verbosity::None, Options::default());
    // Deleting the repository proves the second answer came from the cache and
    // not from the filesystem underneath it.
    std::fs::remove_dir_all(fixture._repository.path()).unwrap();
    let second = fixture.tree(Verbosity::None, Options::default());
    assert_eq!(first, second);
}

#[test]
fn a_sibling_module_resolves_from_the_working_tree() {
    let repository_dir = tempfile::tempdir().unwrap();
    let repository = FakeRepository::new(repository_dir.path());
    let project = tempfile::tempdir().unwrap();

    std::fs::write(
        project.path().join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion>\
           <groupId>com.example</groupId><artifactId>root</artifactId><version>1.0</version>\
           <packaging>pom</packaging>\
           <modules><module>lib</module><module>app</module></modules>\
         </project>",
    )
    .unwrap();
    for (name, body) in [
        ("lib", String::new()),
        (
            "app",
            format!(
                "<dependencies>{}</dependencies>",
                dependency("com.example", "lib", "1.0")
            ),
        ),
    ] {
        let directory = project.path().join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("pom.xml"),
            format!(
                "<project><modelVersion>4.0.0</modelVersion>\
                   <parent><groupId>com.example</groupId><artifactId>root</artifactId>\
                     <version>1.0</version></parent>\
                   <artifactId>{name}</artifactId>{body}</project>"
            ),
        )
        .unwrap();
    }

    let cache = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("settings.xml"), "<settings/>").unwrap();
    let config = Config {
        cache: Some(cache.path().to_path_buf()),
        repositories: Some(vec![repository.as_repository()]),
        user_settings: Some(project.path().join("settings.xml")),
        ..Config::new().without_local_repository()
    };
    let session = Session::new(&config).expect("a session");

    let root = session.project(project.path()).expect("the aggregator");
    assert_eq!(root.modules.len(), 2);

    // `app` depends on `lib`, which has never been deployed anywhere. It has to
    // come from the working tree or the build cannot resolve at all.
    let app = root
        .modules
        .iter()
        .find(|module| module.model.artifact_id.as_deref() == Some("app"))
        .expect("the app module");
    let resolution = session
        .resolve_project(app, Verbosity::None)
        .expect("a resolution");
    let tree = render(
        &resolution.collected.graph,
        Format::Text,
        Options::default(),
    );
    assert_eq!(
        tree,
        "com.example:app:jar:1.0\n\\- com.example:lib:jar:1.0:compile\n"
    );
}

#[test]
fn a_working_tree_module_beats_a_published_one_of_the_same_coordinates() {
    // The POM crawler runs on background threads and writes into the same memo
    // the resolver reads. It knows nothing about the reactor, so when the memo
    // was consulted *before* the reactor a published sibling could win a race
    // and be resolved in place of the module being built.
    //
    // Tested as an invariant rather than as a race: the memo is populated from
    // the repository first, deliberately, so the ordering decides the answer
    // every time. Reproducing the race itself would give a test that passes
    // when it should fail.
    let workspace = tempfile::tempdir().unwrap();
    let repository_dir = workspace.path().join("repo");
    std::fs::create_dir_all(&repository_dir).unwrap();
    let repository = FakeRepository::new(&repository_dir);

    // A *published* com.example:lib:1.0 that pulls in something the working
    // tree's version does not.
    repository.pom(
        "com.example",
        "lib",
        "1.0",
        &format!(
            "<dependencies>{}</dependencies>",
            dependency("org.test", "published", "1.0")
        ),
    );
    repository.pom("org.test", "published", "1.0", "");
    repository.pom("org.test", "fromsource", "1.0", "");

    let project = workspace.path().join("project");
    std::fs::create_dir_all(project.join("lib")).unwrap();
    std::fs::create_dir_all(project.join("app")).unwrap();
    std::fs::write(
        project.join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion>\
           <groupId>com.example</groupId><artifactId>root</artifactId><version>1.0</version>\
           <packaging>pom</packaging>\
           <modules><module>lib</module><module>app</module></modules></project>",
    )
    .unwrap();
    std::fs::write(
        project.join("lib/pom.xml"),
        format!(
            "<project><modelVersion>4.0.0</modelVersion>\
               <parent><groupId>com.example</groupId><artifactId>root</artifactId>\
                 <version>1.0</version></parent>\
               <artifactId>lib</artifactId>\
               <dependencies>{}</dependencies></project>",
            dependency("org.test", "fromsource", "1.0")
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("app/pom.xml"),
        format!(
            "<project><modelVersion>4.0.0</modelVersion>\
               <parent><groupId>com.example</groupId><artifactId>root</artifactId>\
                 <version>1.0</version></parent>\
               <artifactId>app</artifactId>\
               <dependencies>{}</dependencies></project>",
            dependency("com.example", "lib", "1.0")
        ),
    )
    .unwrap();

    let settings = workspace.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();
    let config = Config {
        cache: Some(workspace.path().join("jv-cache")),
        user_settings: Some(settings),
        repositories: Some(vec![repository.as_repository()]),
        ..Config::new().without_local_repository()
    };
    let session = Session::new(&config).expect("a session");

    // Stand in for the crawler having got there first, deterministically.
    session
        .source()
        .effective_model(&jv_model::Artifact::new("com.example", "lib", "1.0"))
        .expect("the published lib")
        .expect("it exists");

    let root = session
        .project_at(&project.join("pom.xml"))
        .expect("a project");
    let app = root
        .modules
        .iter()
        .find(|module| module.model.artifact_id.as_deref() == Some("app"))
        .expect("the app module");

    let tree = render(
        &session
            .resolve_project(app, Verbosity::None)
            .expect("a resolution")
            .collected
            .graph,
        Format::Text,
        Options::default(),
    );
    assert!(
        tree.contains("org.test:fromsource"),
        "the working tree's lib was not used:\n{tree}"
    );
    assert!(
        !tree.contains("org.test:published"),
        "the published lib won over the one being built:\n{tree}"
    );
}

/// A classified sibling is a separate artifact, all the way through the driver.
///
/// The resolver was always right about this; the driver's descriptor cache was
/// keyed `group:artifact:version`, so `g:a:1:data` hit the entry cached for
/// `g:a:1` and came back describing the plain artifact. The collector built its
/// node from that, and conflict resolution dropped the classified one as a
/// duplicate — which is why `xmlresolver:jar:data` went missing from
/// spring-petclinic's checkstyle classpath and `mvn -o` could not run it.
///
/// A resolver-level test cannot catch this: it needs the cache that only the
/// driver has.
#[test]
fn a_classified_sibling_survives_the_descriptor_cache() {
    let workspace = tempfile::tempdir().expect("a temp dir");
    let repository = workspace.path().join("repository");
    let directory = repository.join("g").join("a").join("1");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("a-1.pom"),
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>g</groupId><artifactId>a</artifactId><version>1</version>
</project>"#,
    )
    .unwrap();
    std::fs::write(directory.join("a-1.jar"), b"plain").unwrap();
    std::fs::write(directory.join("a-1-data.jar"), b"data").unwrap();

    let project = workspace.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("pom.xml"),
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId><artifactId>root</artifactId><version>1.0</version>
  <repositories><repository><id>local</id><url>file://{}</url></repository></repositories>
  <dependencies>
    <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1</version></dependency>
    <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1</version><classifier>data</classifier></dependency>
  </dependencies>
</project>"#,
            repository.display()
        ),
    )
    .unwrap();
    let settings = workspace.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();

    let config = Config {
        user_settings: Some(settings),
        cache: Some(workspace.path().join("cache")),
        ignore_local_repository: true,
        ..Config::default()
    };
    let session = Session::new(&config).expect("a session");
    let loaded = session
        .project_at(&project.join("pom.xml"))
        .expect("the project loads");
    let resolution = session
        .resolve_project(&loaded, Verbosity::None)
        .expect("resolution");

    let mut classifiers: Vec<String> = Vec::new();
    let graph = &resolution.collected.graph;
    graph.walk(|id, _depth| {
        if id == graph.root() {
            return;
        }
        let node = graph.node(id);
        if node.omitted_for.is_some() {
            return;
        }
        if let Some(artifact) = &node.artifact {
            classifiers.push(artifact.classifier.clone());
        }
    });
    classifiers.sort();

    assert_eq!(
        classifiers,
        ["", "data"],
        "a classifier selects a different file, so both must survive"
    );
}
