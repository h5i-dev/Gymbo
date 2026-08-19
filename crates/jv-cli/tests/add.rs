//! `jv add` end to end.
//!
//! Nothing here reaches a repository: every case either states a version or
//! relies on `<dependencyManagement>` declared in the POM itself, so the tests
//! say the same thing on a laptop with no network as on a runner with one.
//! Resolving a version from repository metadata is exercised by hand and by the
//! corpus, not here, because a test that needs Central is a test that fails for
//! reasons that have nothing to do with the code.

use std::path::PathBuf;
use std::process::Command;

fn jv_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop();
    path.pop();
    path.join(if cfg!(windows) { "jv.exe" } else { "jv" })
}

struct Project {
    _directory: tempfile::TempDir,
    pom: PathBuf,
}

impl Project {
    fn with(body: &str) -> Self {
        let directory = tempfile::tempdir().expect("a temp dir");
        let pom = directory.path().join("pom.xml");
        std::fs::write(&pom, body).expect("the POM");
        Self {
            _directory: directory,
            pom,
        }
    }

    fn add(&self, arguments: &[&str]) -> (bool, String) {
        let output = Command::new(jv_binary())
            .arg("add")
            .args(arguments)
            .arg("--offline")
            .arg("-f")
            .arg(&self.pom)
            .output()
            .expect("jv should run");
        (
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }

    fn text(&self) -> String {
        std::fs::read_to_string(&self.pom).expect("the POM")
    }
}

const PLAIN: &str = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>

  <!-- a comment nobody wants reformatted -->
  <dependencies>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
      <version>2.0.9</version>
    </dependency>
  </dependencies>
</project>
";

#[test]
fn a_dependency_is_written_and_the_rest_of_the_file_is_not() {
    let project = Project::with(PLAIN);
    let (ok, said) = project.add(&["org.junit.jupiter:junit-jupiter:5.10.2", "--test"]);
    assert!(ok, "{said}");

    let text = project.text();
    assert!(
        text.contains("<artifactId>junit-jupiter</artifactId>"),
        "{text}"
    );
    assert!(text.contains("<scope>test</scope>"), "{text}");
    assert!(
        text.contains("<!-- a comment nobody wants reformatted -->"),
        "the comment was lost:\n{text}"
    );
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
}

#[test]
fn a_dry_run_prints_and_does_not_touch_the_file() {
    let project = Project::with(PLAIN);
    let before = project.text();
    let (ok, said) = project.add(&["org.junit.jupiter:junit-jupiter:5.10.2", "--dry-run"]);
    assert!(ok, "{said}");
    assert!(
        said.contains("junit-jupiter"),
        "nothing was printed:\n{said}"
    );
    assert_eq!(project.text(), before, "--dry-run wrote to the file");
}

#[test]
fn adding_the_same_dependency_twice_says_so_rather_than_duplicating_it() {
    let project = Project::with(PLAIN);
    let (ok, _) = project.add(&["org.slf4j:slf4j-api:2.0.9"]);
    assert!(ok);
    assert_eq!(
        project.text().matches("slf4j-api").count(),
        1,
        "the dependency was duplicated:\n{}",
        project.text()
    );
}

#[test]
fn a_managed_version_is_not_written() {
    // The behaviour the command is judged on. A version here would pin what
    // `<dependencyManagement>` exists to decide, and diverge from every sibling
    // module the next time it moves.
    let project = Project::with(
        "\
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>org.slf4j</groupId>
        <artifactId>slf4j-api</artifactId>
        <version>2.0.9</version>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
  </dependencies>
</project>
",
    );
    let (ok, said) = project.add(&["org.slf4j:slf4j-api"]);
    assert!(ok, "{said}");
    assert!(
        said.contains("already managed"),
        "the reason was not explained:\n{said}"
    );

    let text = project.text();
    let start = text.rfind("<dependencies>").expect("the block");
    let added = &text[start..];
    assert!(
        added.contains("<artifactId>slf4j-api</artifactId>"),
        "{text}"
    );
    assert!(
        !added.contains("<version>"),
        "a version was written for a managed dependency:\n{text}"
    );
}

#[test]
fn malformed_coordinates_are_refused_by_name() {
    let project = Project::with(PLAIN);
    let (ok, said) = project.add(&["just-a-name"]);
    assert!(!ok);
    assert!(said.contains("just-a-name"), "{said}");
    assert!(said.contains("group:artifact"), "{said}");
}

#[test]
fn an_unknown_module_lists_the_ones_there_are() {
    let project = Project::with(PLAIN);
    let (ok, said) = project.add(&["g:a:1", "-m", "nosuch"]);
    assert!(!ok);
    assert!(said.contains("nosuch"), "{said}");
    assert!(
        said.contains("demo"),
        "the available modules were not offered:\n{said}"
    );
}

#[test]
fn a_scope_and_a_classifier_are_written_in_mavens_order() {
    let project = Project::with(PLAIN);
    let (ok, said) = project.add(&[
        "org.example:thing:1.0",
        "--scope",
        "provided",
        "--classifier",
        "linux-x86_64",
        "--type",
        "so",
        "--optional",
    ]);
    assert!(ok, "{said}");

    let text = project.text();
    let start = text.find("<artifactId>thing<").expect("the addition");
    let tail = &text[start..];
    let version = tail.find("<version>").expect("version");
    let classifier = tail.find("<classifier>").expect("classifier");
    let type_ = tail.find("<type>").expect("type");
    let scope = tail.find("<scope>").expect("scope");
    let optional = tail.find("<optional>").expect("optional");
    assert!(
        version < classifier && classifier < type_ && type_ < scope && scope < optional,
        "written out of Maven's order:\n{tail}"
    );
}

// ---------------------------------------------------------------- removing

impl Project {
    fn remove(&self, arguments: &[&str]) -> (bool, String) {
        let output = Command::new(jv_binary())
            .arg("remove")
            .args(arguments)
            .arg("--offline")
            .arg("-f")
            .arg(&self.pom)
            .output()
            .expect("jv should run");
        (
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }
}

#[test]
fn removing_takes_the_dependency_and_leaves_the_file_alone() {
    let project = Project::with(PLAIN);
    let (ok, said) = project.remove(&["org.slf4j:slf4j-api"]);
    assert!(ok, "{said}");

    let text = project.text();
    assert!(!text.contains("slf4j"), "{text}");
    assert!(
        text.contains("<!-- a comment nobody wants reformatted -->"),
        "the comment was lost:\n{text}"
    );
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(
        !text
            .lines()
            .any(|line| !line.is_empty() && line.trim().is_empty()),
        "a blank line was left behind:\n{text:?}"
    );
}

#[test]
fn a_version_in_the_argument_is_tolerated() {
    // So a line copied from `jv add` works rather than failing on an argument
    // that reads as perfectly correct.
    let project = Project::with(PLAIN);
    let (ok, said) = project.remove(&["org.slf4j:slf4j-api:2.0.9"]);
    assert!(ok, "{said}");
    assert!(!project.text().contains("slf4j"), "{}", project.text());
}

#[test]
fn removing_something_absent_says_so_and_succeeds() {
    let project = Project::with(PLAIN);
    let before = project.text();
    let (ok, said) = project.remove(&["org.example:absent"]);
    assert!(ok, "removing what is not there is not an error: {said}");
    assert!(said.contains("not a dependency"), "{said}");
    assert_eq!(project.text(), before);
}

#[test]
fn remove_dry_run_does_not_write() {
    let project = Project::with(PLAIN);
    let before = project.text();
    let (ok, said) = project.remove(&["org.slf4j:slf4j-api", "--dry-run"]);
    assert!(ok, "{said}");
    assert_eq!(project.text(), before, "--dry-run wrote to the file");
}
