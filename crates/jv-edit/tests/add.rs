//! Adding a dependency without disturbing anything else in the file.
//!
//! The property under test throughout is that the output is the input with one
//! `<dependency>` element inserted and *nothing else changed*. Several tests
//! assert that directly by deleting the inserted span and comparing against the
//! original, which catches reformatting that an eyeball comparison would miss.

use jv_edit::{Added, Dependency, add_dependency};

fn junit() -> Dependency {
    Dependency {
        group_id: "org.junit.jupiter".to_owned(),
        artifact_id: "junit-jupiter".to_owned(),
        version: Some("5.10.2".to_owned()),
        scope: Some("test".to_owned()),
        ..Dependency::default()
    }
}

fn inserted(pom: &str, dependency: &Dependency) -> String {
    match add_dependency(pom, dependency).expect("a POM") {
        Added::Inserted(text) => text,
        other => panic!("expected an insertion, got {other:?}"),
    }
}

/// The original text, recovered by deleting whatever the edit added.
///
/// If this differs from the input, the edit reformatted something.
fn without_the_addition(before: &str, after: &str) -> String {
    let common = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    let tail = before[common..].len();
    format!("{}{}", &after[..common], &after[after.len() - tail..])
}

#[test]
fn a_dependency_is_added_and_nothing_else_moves() {
    let pom = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<project xmlns=\"http://maven.apache.org/POM/4.0.0\">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>

  <!-- what we depend on, and why -->
  <dependencies>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
      <version>2.0.9</version>
    </dependency>
  </dependencies>
</project>
";
    let after = inserted(pom, &junit());

    assert!(after.contains("<artifactId>junit-jupiter</artifactId>"));
    assert!(after.contains("<scope>test</scope>"));
    assert_eq!(
        without_the_addition(pom, &after),
        pom,
        "the edit changed something other than the addition"
    );
    // The comment and the declaration are the things a reserialiser eats.
    assert!(after.contains("<!-- what we depend on, and why -->"));
    assert!(after.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
}

#[test]
fn the_files_own_indentation_is_matched() {
    let pom = "\
<project>
    <artifactId>demo</artifactId>
    <dependencies>
        <dependency>
            <groupId>org.slf4j</groupId>
            <artifactId>slf4j-api</artifactId>
        </dependency>
    </dependencies>
</project>
";
    let after = inserted(pom, &junit());
    assert!(
        after.contains("        <dependency>\n            <groupId>org.junit.jupiter</groupId>"),
        "four-space indentation was not matched:\n{after}"
    );
    assert_eq!(without_the_addition(pom, &after), pom);
}

#[test]
fn tabs_stay_tabs() {
    let pom = "<project>\n\t<artifactId>demo</artifactId>\n\t<dependencies>\n\t\t<dependency>\n\t\t\t<groupId>org.slf4j</groupId>\n\t\t</dependency>\n\t</dependencies>\n</project>\n";
    let after = inserted(pom, &junit());
    assert!(
        after.contains("\t\t<dependency>\n\t\t\t<groupId>org.junit.jupiter</groupId>"),
        "tab indentation was not matched:\n{after:?}"
    );
    assert!(
        !after.contains("\n  "),
        "spaces leaked into a tab-indented file"
    );
}

#[test]
fn windows_line_endings_survive() {
    let pom = "<project>\r\n  <artifactId>demo</artifactId>\r\n  <dependencies>\r\n    <dependency>\r\n      <groupId>org.slf4j</groupId>\r\n    </dependency>\r\n  </dependencies>\r\n</project>\r\n";
    let after = inserted(pom, &junit());
    assert!(
        after.matches('\n').count() == after.matches("\r\n").count(),
        "a bare newline was introduced into a CRLF file:\n{after:?}"
    );
    assert_eq!(without_the_addition(pom, &after), pom);
}

#[test]
fn a_pom_with_no_dependencies_gets_the_element() {
    let pom = "\
<project>
  <modelVersion>4.0.0</modelVersion>
  <artifactId>demo</artifactId>
</project>
";
    let after = inserted(pom, &junit());
    assert!(
        after.contains("  <dependencies>\n    <dependency>"),
        "{after}"
    );
    assert!(after.contains("  </dependencies>\n</project>"), "{after}");
    assert_eq!(without_the_addition(pom, &after), pom);
}

#[test]
fn an_empty_dependencies_element_is_filled_in() {
    let pom =
        "<project>\n  <artifactId>demo</artifactId>\n  <dependencies></dependencies>\n</project>\n";
    let after = inserted(pom, &junit());
    assert!(
        after.contains("<dependencies>\n    <dependency>"),
        "{after}"
    );
    assert!(
        after.contains("</dependency>\n  </dependencies>"),
        "{after}"
    );
}

#[test]
fn a_dependency_already_there_is_reported_not_duplicated() {
    // Maven warns about a duplicated declaration and then honours the last,
    // which is a confusing state for a tool to have created.
    let pom = "\
<project>
  <artifactId>demo</artifactId>
  <dependencies>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.9.0</version>
    </dependency>
  </dependencies>
</project>
";
    match add_dependency(pom, &junit()).expect("a POM") {
        Added::AlreadyPresent { line, version } => {
            assert_eq!(version.as_deref(), Some("5.9.0"));
            assert_eq!(line, 4, "should point at the existing declaration");
        }
        other => panic!("expected AlreadyPresent, got {other:?}"),
    }
}

#[test]
fn a_plugins_dependencies_block_is_not_mistaken_for_the_projects() {
    // `<plugin><dependencies>` and `<dependencyManagement><dependencies>` are
    // the two traps. Writing into either would change what the build does
    // without adding the dependency the user asked for.
    let pom = "\
<project>
  <artifactId>demo</artifactId>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>com.fasterxml.jackson</groupId>
        <artifactId>jackson-bom</artifactId>
        <version>2.16.1</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <build>
    <plugins>
      <plugin>
        <artifactId>maven-compiler-plugin</artifactId>
        <dependencies>
          <dependency>
            <groupId>org.postgresql</groupId>
            <artifactId>postgresql</artifactId>
          </dependency>
        </dependencies>
      </plugin>
    </plugins>
  </build>
</project>
";
    let after = inserted(pom, &junit());

    // The new entry is in a project-level block created for it, after the
    // build section, not inside either existing `<dependencies>`.
    let added = after.find("junit-jupiter").expect("the addition");
    let management = after.find("jackson-bom").expect("the bom");
    let plugin = after.find("postgresql").expect("the plugin dependency");
    assert!(
        added > management && added > plugin,
        "added in the wrong block:\n{after}"
    );
    assert_eq!(without_the_addition(pom, &after), pom);
}

#[test]
fn a_managed_dependency_is_written_without_a_version() {
    // The case the whole design turns on: a version here would pin what the
    // project deliberately left to its BOM.
    let pom = "<project>\n  <artifactId>demo</artifactId>\n  <dependencies>\n  </dependencies>\n</project>\n";
    let managed = Dependency {
        group_id: "com.fasterxml.jackson.core".to_owned(),
        artifact_id: "jackson-databind".to_owned(),
        version: None,
        ..Dependency::default()
    };
    let after = inserted(pom, &managed);
    assert!(after.contains("<artifactId>jackson-databind</artifactId>"));
    assert!(
        !after.contains("<version>"),
        "a version was invented for a managed dependency:\n{after}"
    );
}

#[test]
fn the_element_order_is_maven_s_own() {
    let pom = "<project>\n  <artifactId>demo</artifactId>\n  <dependencies>\n  </dependencies>\n</project>\n";
    let full = Dependency {
        group_id: "g".to_owned(),
        artifact_id: "a".to_owned(),
        version: Some("1".to_owned()),
        classifier: Some("tests".to_owned()),
        type_: Some("test-jar".to_owned()),
        scope: Some("test".to_owned()),
        optional: true,
    };
    let after = inserted(pom, &full);
    // Only inside the element that was added: the project has an `<artifactId>`
    // of its own, earlier in the file, and searching the whole text finds that
    // one instead.
    let start = after.find("<dependency>").expect("the addition");
    let end = after.find("</dependency>").expect("the addition");
    let element = &after[start..end];

    let mut last = 0;
    for name in [
        "groupId",
        "artifactId",
        "version",
        "classifier",
        "type",
        "scope",
        "optional",
    ] {
        let at = element
            .find(&format!("<{name}>"))
            .unwrap_or_else(|| panic!("missing <{name}>:\n{element}"));
        assert!(at > last, "<{name}> is out of order:\n{element}");
        last = at;
    }
}

#[test]
fn something_that_is_not_a_pom_is_refused() {
    let error = add_dependency("<settings><servers/></settings>", &junit()).unwrap_err();
    assert!(error.to_string().contains("settings"), "{error}");
}

#[test]
fn a_version_that_is_a_property_expression_is_written_as_written() {
    let pom = "<project>\n  <artifactId>demo</artifactId>\n  <dependencies>\n  </dependencies>\n</project>\n";
    let with_property = Dependency {
        version: Some("${junit.version}".to_owned()),
        ..junit()
    };
    let after = inserted(pom, &with_property);
    assert!(
        after.contains("<version>${junit.version}</version>"),
        "{after}"
    );
}

#[test]
fn an_empty_block_written_across_lines_leaves_no_blank_line() {
    // `<dependencies>\n  </dependencies>` holds only whitespace, and that
    // whitespace is not the right shape once the element has a child.
    let pom = "<project>\n  <artifactId>demo</artifactId>\n  <dependencies>\n  </dependencies>\n</project>\n";
    let after = inserted(pom, &junit());
    assert!(
        !after
            .lines()
            .any(|line| !line.is_empty() && line.trim().is_empty()),
        "a whitespace-only line was left behind:\n{after:?}"
    );
    assert!(
        after.contains("</dependency>\n  </dependencies>"),
        "{after}"
    );
}

// --------------------------------------------------------------- removing

use jv_edit::{Removed, remove_dependency};

fn removed(pom: &str, group_id: &str, artifact_id: &str) -> String {
    match remove_dependency(pom, group_id, artifact_id).expect("a POM") {
        Removed::Removed(text) => text,
        Removed::NotPresent => panic!("expected a removal"),
    }
}

const TWO: &str = "\
<?xml version=\"1.0\"?>
<project>
  <artifactId>demo</artifactId>
  <dependencies>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
      <version>2.0.9</version>
    </dependency>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>33.4.8-jre</version>
    </dependency>
  </dependencies>
</project>
";

#[test]
fn removing_takes_the_element_and_its_line_and_nothing_else() {
    let after = removed(TWO, "org.slf4j", "slf4j-api");
    assert!(!after.contains("slf4j"), "the element survived:\n{after}");
    assert!(after.contains("guava"), "the wrong element went:\n{after}");
    assert!(
        !after
            .lines()
            .any(|line| !line.is_empty() && line.trim().is_empty()),
        "a blank line was left behind:\n{after:?}"
    );
    // Adding it back reproduces the original exactly, which is the strongest
    // statement that removal disturbed nothing.
    let restored = inserted(
        &after,
        &Dependency {
            group_id: "org.slf4j".to_owned(),
            artifact_id: "slf4j-api".to_owned(),
            version: Some("2.0.9".to_owned()),
            ..Dependency::default()
        },
    );
    assert!(restored.contains("<artifactId>slf4j-api</artifactId>"));
}

#[test]
fn removing_the_last_one_leaves_the_block() {
    // An empty `<dependencies>` is valid, and it may hold comments. Removing
    // it would be a second edit nobody asked for.
    let pom = "<project>\n  <artifactId>demo</artifactId>\n  <dependencies>\n    <dependency>\n      <groupId>g</groupId>\n      <artifactId>a</artifactId>\n    </dependency>\n  </dependencies>\n</project>\n";
    let after = removed(pom, "g", "a");
    assert!(after.contains("<dependencies>"), "{after}");
    assert!(after.contains("</dependencies>"), "{after}");
    assert!(!after.contains("<dependency>"), "{after}");
}

#[test]
fn removing_something_absent_changes_nothing() {
    assert_eq!(
        remove_dependency(TWO, "org.example", "nothing").expect("a POM"),
        Removed::NotPresent
    );
}

#[test]
fn a_managed_or_plugin_dependency_is_not_removed_by_mistake() {
    let pom = "\
<project>
  <artifactId>demo</artifactId>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>org.slf4j</groupId>
        <artifactId>slf4j-api</artifactId>
        <version>2.0.9</version>
      </dependency>
    </dependencies>
  </dependencyManagement>
</project>
";
    // Declared only in management, so there is nothing to remove from the
    // project's own dependencies — and management must be left alone.
    assert_eq!(
        remove_dependency(pom, "org.slf4j", "slf4j-api").expect("a POM"),
        Removed::NotPresent
    );
}

#[test]
fn a_comment_above_the_entry_is_left_alone() {
    // Guessing which comments belong to which element is how an editing tool
    // deletes a note somebody needed.
    let pom = "\
<project>
  <artifactId>demo</artifactId>
  <dependencies>
    <!-- needed until the migration lands -->
    <dependency>
      <groupId>g</groupId>
      <artifactId>a</artifactId>
    </dependency>
  </dependencies>
</project>
";
    let after = removed(pom, "g", "a");
    assert!(
        after.contains("<!-- needed until the migration lands -->"),
        "the comment was taken with it:\n{after}"
    );
}
