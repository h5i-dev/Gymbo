//! `.mvn/` — the project-local Maven configuration jv used to ignore entirely.
//!
//! A directory containing `.mvn/` is what Maven calls the *multi-module project
//! directory*, found by walking up from the working directory. Two files in it
//! change what a build resolves, and missing them is a silent-divergence bug
//! rather than a missing feature: a project that puts `-Dsomething=value` in
//! `.mvn/maven.config` gets one dependency graph under `mvn` and a different
//! one under a tool that does not read it, with nothing in either output
//! saying why.
//!
//! # `maven.config` parsing, as Maven 3.9.9 actually does it
//!
//! Established by running Maven 3.9.9 against a POM whose profile activates on
//! a property, and watching whether the dependency appeared:
//!
//! | File contents | Result |
//! |---|---|
//! | `-Dactivator=on` | applied |
//! | `-Dactivator=on -Dother=x` | **not** applied — the whole line is one argument |
//! | `# comment` then `-Dactivator=on` | applied; `#` lines are skipped |
//! | blank lines around the argument | applied; empty lines are skipped |
//! | `  -Dactivator=on  ` (padded) | **not** applied — lines are not trimmed |
//!
//! So: one argument per line, taken verbatim, skipping empty lines and those
//! starting with `#`. The no-trimming rule is the surprising one, and copying
//! it matters — a tool that trims would accept a file Maven silently ignores,
//! and would then resolve differently from the build it is imitating.
//!
//! Command-line arguments win over `maven.config` on conflict — checked against
//! Maven 3.9.9 — which falls out of parsing the file's arguments first and
//! letting the last occurrence take effect. jv's CLI is subcommand-based where
//! `mvn`'s is flat, so they are spliced in after the subcommand rather than at
//! the front; see [`apply_to_command_line`].
//!
//! Only options that affect *resolution* are carried over. A real
//! `maven.config` is full of build options (`-T 1C`, `-e`, `--fail-at-end`)
//! that jv has no equivalent for, and failing on those would turn a file jv
//! used to ignore into a hard error.

use std::path::{Path, PathBuf};

/// The directory holding `.mvn/`, found by walking up from `start`.
///
/// Maven's launcher script does this from the working directory, and so does
/// this: `-f` pointing elsewhere does not move it.
pub fn project_directory(start: &Path) -> Option<PathBuf> {
    let mut directory = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    loop {
        if directory.join(".mvn").is_dir() {
            return Some(directory);
        }
        if !directory.pop() {
            return None;
        }
    }
}

/// The arguments in `<project>/.mvn/maven.config`, in file order.
///
/// An unreadable file yields no arguments rather than an error: Maven treats a
/// missing one as absent, and a build should not fail because a config file is
/// unreadable when the resolve would otherwise have succeeded.
pub fn config_args(project_directory: &Path) -> Vec<String> {
    let path = project_directory.join(".mvn").join("maven.config");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_config(&contents)
}

/// Splits the file into arguments. See the module docs for why this neither
/// trims nor splits on whitespace.
fn parse_config(contents: &str) -> Vec<String> {
    contents
        .lines()
        // `\r` would otherwise become part of the argument on a file written
        // on Windows, which Maven's line-based read also drops.
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// A core extension declared in `.mvn/extensions.xml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoreExtension {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
}

impl CoreExtension {
    /// `groupId:artifactId:version`, for messages.
    pub fn coordinates(&self) -> String {
        format!("{}:{}:{}", self.group_id, self.artifact_id, self.version)
    }
}

/// The extensions declared in `<project>/.mvn/extensions.xml`.
///
/// jv cannot *run* a core extension — that needs a JVM and Maven's own plugin
/// container. It can and must still resolve and download them, because
/// `mvn -o` fails outright when an extension is missing from the local
/// repository, and that failure comes before anything a dependency graph could
/// explain.
pub fn extensions(project_directory: &Path) -> Vec<CoreExtension> {
    let path = project_directory.join(".mvn").join("extensions.xml");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_extensions(&contents)
}

fn parse_extensions(xml: &str) -> Vec<CoreExtension> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut found = Vec::new();
    let mut current = CoreExtension::default();
    let mut path: Vec<String> = Vec::new();
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "extension" {
                    current = CoreExtension::default();
                }
                path.push(name);
            }
            Ok(quick_xml::events::Event::End(element)) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if name == "extension" && !current.group_id.is_empty() {
                    found.push(std::mem::take(&mut current));
                }
                path.pop();
            }
            Ok(quick_xml::events::Event::Text(text)) => {
                let value = text.unescape().unwrap_or_default().trim().to_string();
                if value.is_empty() {
                    continue;
                }
                if let [.., parent, field] = path.as_slice()
                    && parent == "extension"
                {
                    match field.as_str() {
                        "groupId" => current.group_id = value,
                        "artifactId" => current.artifact_id = value,
                        "version" => current.version = value,
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    found
}

/// The `maven.config` options jv understands, translated to jv's spelling.
///
/// `maven.config` is written for `mvn`, so it routinely carries options that
/// mean nothing here — `-T 1C`, `-e`, `--fail-at-end`, `-Dstyle.color`. Passing
/// those through would abort the run on an unknown argument, turning a file jv
/// previously ignored into a hard failure, which is a worse regression than the
/// bug being fixed. Only options that change *resolution* are taken, and the
/// rest are dropped.
///
/// Maven's `-P` takes a comma-separated list and jv's takes one profile per
/// flag, so a list is expanded.
fn translate(args: &[String]) -> Vec<String> {
    let mut translated = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        index += 1;

        // Options that carry their value in the same token.
        if let Some(property) = argument.strip_prefix("-D") {
            if !property.is_empty() {
                translated.push(format!("-D{property}"));
            }
            continue;
        }

        // `-P a,b` and `--activate-profiles a,b`, value in the next token.
        if matches!(argument, "-P" | "--activate-profiles") {
            if let Some(list) = args.get(index) {
                index += 1;
                for profile in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    translated.push("-P".to_owned());
                    translated.push(profile.to_owned());
                }
            }
            continue;
        }
        if let Some(list) = argument.strip_prefix("-P").filter(|list| !list.is_empty()) {
            for profile in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                translated.push("-P".to_owned());
                translated.push(profile.to_owned());
            }
            continue;
        }

        match argument {
            "-o" | "--offline" => translated.push("-o".to_owned()),
            "-U" | "--update-snapshots" => translated.push("-U".to_owned()),
            "-s" | "--settings" => {
                if let Some(value) = args.get(index) {
                    index += 1;
                    translated.push("-s".to_owned());
                    translated.push(value.clone());
                }
            }
            "-gs" | "--global-settings" => {
                if let Some(value) = args.get(index) {
                    index += 1;
                    translated.push("--global-settings".to_owned());
                    translated.push(value.clone());
                }
            }
            // Anything else is a build option jv has no equivalent for.
            _ => {}
        }
    }
    translated
}

/// Splices `maven.config` arguments into a command line.
///
/// They go immediately after the subcommand, so they are parsed before the
/// user's own flags and lose to them on conflict — Maven's precedence. jv's CLI
/// is subcommand-based where `mvn`'s is flat, so they cannot simply be
/// prepended: `-D` is not valid before the subcommand name.
///
/// With no subcommand present (`jv --version`, or no arguments) nothing is
/// inserted, since there is nothing for the options to apply to.
pub fn apply_to_command_line<I>(argv: I, working_directory: &Path) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let argv: Vec<String> = argv.into_iter().collect();
    let Some(directory) = project_directory(working_directory) else {
        return argv;
    };
    let translated = translate(&config_args(&directory));
    if translated.is_empty() {
        return argv;
    }

    // The subcommand is the first argument after the program name that is not
    // a flag.
    let Some(position) = argv
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, argument)| !argument.starts_with('-'))
        .map(|(index, _)| index)
    else {
        return argv;
    };

    let mut combined = argv[..=position].to_vec();
    combined.extend(translated);
    combined.extend_from_slice(&argv[position + 1..]);
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each case below was checked against Maven 3.9.9 before it was written
    // here; see the table in the module docs.

    #[test]
    fn one_argument_per_line() {
        assert_eq!(parse_config("-Dfoo=bar\n-Pdev\n"), ["-Dfoo=bar", "-Pdev"]);
    }

    #[test]
    fn a_line_is_one_argument_even_when_it_contains_spaces() {
        // Maven does not tokenise the line, which is why this file does not
        // either: `-Da=1 -Db=2` on one line is a single unparseable argument
        // and Maven silently ignores it.
        assert_eq!(parse_config("-Da=1 -Db=2\n"), ["-Da=1 -Db=2"]);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        assert_eq!(
            parse_config("# a note\n\n-Dfoo=bar\n\n# trailing\n"),
            ["-Dfoo=bar"]
        );
    }

    #[test]
    fn lines_are_not_trimmed() {
        // The surprising rule, and the reason for copying it: Maven ignores a
        // padded argument, so trimming here would make jv honour a line the
        // build it is imitating throws away.
        assert_eq!(parse_config("  -Dfoo=bar  \n"), ["  -Dfoo=bar  "]);
    }

    #[test]
    fn carriage_returns_do_not_become_part_of_the_argument() {
        assert_eq!(parse_config("-Dfoo=bar\r\n"), ["-Dfoo=bar"]);
    }

    #[test]
    fn a_hash_only_counts_at_the_start_of_a_line() {
        assert_eq!(parse_config("-Dfoo=a#b\n"), ["-Dfoo=a#b"]);
    }

    #[test]
    fn extensions_are_read_with_their_coordinates() {
        let found = parse_extensions(
            r#"<extensions>
                 <extension>
                   <groupId>io.takari.maven</groupId>
                   <artifactId>takari-smart-builder</artifactId>
                   <version>0.6.5</version>
                 </extension>
                 <extension>
                   <groupId>com.example</groupId>
                   <artifactId>other</artifactId>
                   <version>1.0</version>
                 </extension>
               </extensions>"#,
        );
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].coordinates(),
            "io.takari.maven:takari-smart-builder:0.6.5"
        );
        assert_eq!(found[1].coordinates(), "com.example:other:1.0");
    }

    #[test]
    fn an_empty_or_broken_extensions_file_yields_nothing() {
        assert!(parse_extensions("").is_empty());
        assert!(parse_extensions("<extensions></extensions>").is_empty());
        // Truncated: the reader stops, and a half-read entry is not reported.
        assert!(parse_extensions("<extensions><extension><groupId>g").is_empty());
    }

    #[test]
    fn the_project_directory_is_the_one_holding_dot_mvn() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(root.path().join(".mvn")).unwrap();

        assert_eq!(
            project_directory(&nested).as_deref(),
            Some(root.path()),
            "the search must walk up, since Maven's launcher does"
        );
        assert_eq!(project_directory(root.path()).as_deref(), Some(root.path()));
    }

    #[test]
    fn no_dot_mvn_anywhere_is_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        // A temp dir has no `.mvn` above it in any realistic checkout, but the
        // walk still has to terminate rather than loop at the filesystem root.
        let _ = project_directory(root.path());
    }

    #[test]
    fn config_arguments_go_after_the_subcommand_and_before_the_users_flags() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".mvn")).unwrap();
        std::fs::write(
            root.path().join(".mvn").join("maven.config"),
            "-Dactivator=on\n",
        )
        .unwrap();

        let combined = apply_to_command_line(
            ["jv", "tree", "-Dactivator=off"].map(str::to_owned),
            root.path(),
        );
        assert_eq!(
            combined,
            ["jv", "tree", "-Dactivator=on", "-Dactivator=off"],
            "jv's CLI is subcommand-based, so the options belong after the \
             subcommand — and before the user's, so the user's win"
        );
    }

    #[test]
    fn options_jv_has_no_equivalent_for_are_dropped_rather_than_fatal() {
        // A real `maven.config` carries build options. Passing them through
        // would abort on an unknown argument, which is a worse regression than
        // ignoring the file altogether.
        let translated = translate(
            &[
                "-T",
                "1C",
                "-e",
                "--fail-at-end",
                "-Dfoo=bar",
                "--batch-mode",
            ]
            .map(str::to_owned),
        );
        assert_eq!(translated, ["-Dfoo=bar"]);
    }

    #[test]
    fn resolution_affecting_options_are_translated() {
        assert_eq!(
            translate(&["-o", "-U", "-Dfoo=bar"].map(str::to_owned)),
            ["-o", "-U", "-Dfoo=bar"]
        );
        assert_eq!(
            translate(&["-s", "/tmp/s.xml"].map(str::to_owned)),
            ["-s", "/tmp/s.xml"]
        );
        assert_eq!(
            translate(&["--global-settings", "/tmp/g.xml"].map(str::to_owned)),
            ["--global-settings", "/tmp/g.xml"]
        );
    }

    #[test]
    fn a_comma_separated_profile_list_becomes_one_flag_each() {
        // Maven takes a list; jv takes one per flag.
        assert_eq!(
            translate(&["-Pdev,fast".to_owned()]),
            ["-P", "dev", "-P", "fast"]
        );
        assert_eq!(
            translate(&["-P".to_owned(), "dev, fast".to_owned()]),
            ["-P", "dev", "-P", "fast"]
        );
    }

    #[test]
    fn nothing_is_inserted_without_a_subcommand() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".mvn")).unwrap();
        std::fs::write(root.path().join(".mvn").join("maven.config"), "-Dfoo=bar\n").unwrap();

        // `jv --version` has nothing for the options to attach to, and adding
        // them would turn a working command into a parse error.
        assert_eq!(
            apply_to_command_line(["jv", "--version"].map(str::to_owned), root.path()),
            ["jv", "--version"]
        );
    }
}
