//! The command line.
//!
//! Flag names follow Maven's where Maven has one, because the people who will
//! try jv already have `-o`, `-U`, `-s` and `-Dkey=value` in their fingers, and
//! a tool that renames them for the sake of it makes itself harder to adopt for
//! no gain.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use jv_repo::UpdatePolicy;
use jv_tree::Format;

/// An extremely fast JVM package and toolchain manager.
#[derive(Debug, Parser)]
#[command(name = "jv", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the dependency tree.
    Tree(TreeArgs),
    /// List the resolved dependencies, one per line.
    Resolve(ResolveArgs),
}

/// Options every command that resolves shares.
#[derive(Args, Debug, Clone)]
pub struct CommonArgs {
    /// The pom.xml to read. Defaults to the nearest one at or above the working
    /// directory.
    #[arg(short = 'f', long = "file", value_name = "POM")]
    pub file: Option<PathBuf>,

    /// Work offline; fail rather than contact a repository.
    #[arg(short = 'o', long)]
    pub offline: bool,

    /// Check for updated releases and snapshots, ignoring the cached copy.
    #[arg(short = 'U', long = "update-snapshots")]
    pub update: bool,

    /// Never check for updates; use whatever is cached.
    #[arg(long, conflicts_with = "update")]
    pub no_update: bool,

    /// An alternative user settings.xml.
    #[arg(short = 's', long = "settings", value_name = "FILE")]
    pub settings: Option<PathBuf>,

    /// An alternative installation settings.xml.
    #[arg(long = "global-settings", value_name = "FILE")]
    pub global_settings: Option<PathBuf>,

    /// jv's cache directory.
    #[arg(long, value_name = "DIR", env = "JV_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Do not read Maven's ~/.m2/repository.
    #[arg(long)]
    pub no_local_repository: bool,

    /// The Java version `<jdk>` profile activators match against. Detected from
    /// JAVA_HOME or `java` when absent.
    #[arg(long, value_name = "VERSION")]
    pub java_version: Option<String>,

    /// Define a property, as `-Dkey=value`.
    #[arg(short = 'D', value_name = "KEY=VALUE")]
    pub define: Vec<String>,

    /// Activate a profile. Prefix with `!` to deactivate one.
    #[arg(
        short = 'P',
        long = "profile",
        value_name = "ID",
        value_delimiter = ','
    )]
    pub profiles: Vec<String>,
}

impl CommonArgs {
    /// Splits `-D` values into pairs.
    ///
    /// A bare `-Dflag` is `flag=true`, which is what Java does and what POMs
    /// that test for a property's presence expect.
    pub fn properties(&self) -> Vec<(String, String)> {
        self.define
            .iter()
            .map(|definition| match definition.split_once('=') {
                Some((key, value)) => (key.to_owned(), value.to_owned()),
                None => (definition.clone(), "true".to_owned()),
            })
            .collect()
    }

    /// Profile ids to force on.
    pub fn active_profiles(&self) -> Vec<String> {
        self.profiles
            .iter()
            .filter(|id| !id.starts_with('!') && !id.starts_with('-'))
            .cloned()
            .collect()
    }

    /// Profile ids to force off, with the `!` or `-` prefix removed.
    pub fn inactive_profiles(&self) -> Vec<String> {
        self.profiles
            .iter()
            .filter_map(|id| id.strip_prefix('!').or_else(|| id.strip_prefix('-')))
            .map(str::to_owned)
            .collect()
    }

    /// The update policy these flags ask for, if they ask for one.
    pub fn update_policy(&self) -> Option<UpdatePolicy> {
        if self.update {
            Some(UpdatePolicy::Always)
        } else if self.no_update {
            Some(UpdatePolicy::Never)
        } else {
            None
        }
    }
}

#[derive(Args, Debug)]
pub struct TreeArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Show why each version was chosen, and which ones lost.
    #[arg(long)]
    pub verbose: bool,

    /// The output format.
    #[arg(
        long = "output-type",
        short = 't',
        value_name = "TYPE",
        default_value = "text"
    )]
    pub output_type: Format,

    /// Write to a file instead of standard output.
    #[arg(long = "output-file", short = 'O', value_name = "FILE")]
    pub output_file: Option<PathBuf>,

    /// The indent characters to draw the tree with.
    #[arg(long, value_enum, default_value_t = TokenStyle::Standard)]
    pub tokens: TokenStyle,

    /// Resolve every module of a multi-module build rather than just the one in
    /// the working directory.
    #[arg(long)]
    pub recursive: bool,
}

#[derive(Args, Debug)]
pub struct ResolveArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Download the artifacts and print their paths in the cache.
    #[arg(long)]
    pub paths: bool,

    /// Print a classpath instead of one artifact per line.
    #[arg(long, conflicts_with = "paths")]
    pub classpath: bool,

    /// Only include dependencies in this scope's resolution.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Resolve every module of a multi-module build.
    #[arg(long)]
    pub recursive: bool,
}

/// The `--tokens` spellings, matching `dependency:tree`'s `-Dtokens`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum TokenStyle {
    #[default]
    Standard,
    Whitespace,
    Extended,
}

impl From<TokenStyle> for jv_tree::Tokens {
    fn from(style: TokenStyle) -> Self {
        match style {
            TokenStyle::Standard => jv_tree::Tokens::Standard,
            TokenStyle::Whitespace => jv_tree::Tokens::Whitespace,
            TokenStyle::Extended => jv_tree::Tokens::Extended,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("valid arguments")
    }

    #[test]
    fn the_command_definition_is_valid() {
        // clap's own consistency check: duplicate flags, bad conflicts, and
        // impossible defaults all surface here rather than at a user's terminal.
        Cli::command().debug_assert();
    }

    #[test]
    fn a_bare_property_is_true() {
        let cli = parse(&["jv", "tree", "-Dci", "-Dver=2.0"]);
        let Command::Tree(tree) = cli.command else {
            panic!("expected tree")
        };
        assert_eq!(
            tree.common.properties(),
            [
                ("ci".to_owned(), "true".to_owned()),
                ("ver".to_owned(), "2.0".to_owned())
            ]
        );
    }

    #[test]
    fn a_value_containing_an_equals_sign_survives() {
        let cli = parse(&["jv", "tree", "-Durl=https://a?b=c"]);
        let Command::Tree(tree) = cli.command else {
            panic!("expected tree")
        };
        assert_eq!(tree.common.properties()[0].1, "https://a?b=c");
    }

    #[test]
    fn profiles_split_into_on_and_off() {
        let cli = parse(&["jv", "tree", "-P", "release,!slow,-also-off"]);
        let Command::Tree(tree) = cli.command else {
            panic!("expected tree")
        };
        assert_eq!(tree.common.active_profiles(), ["release"]);
        // Both spellings Maven accepts for turning a profile off.
        assert_eq!(tree.common.inactive_profiles(), ["slow", "also-off"]);
    }

    #[test]
    fn update_flags_map_to_policies() {
        let cli = parse(&["jv", "tree", "-U"]);
        let Command::Tree(tree) = cli.command else {
            panic!("expected tree")
        };
        assert_eq!(tree.common.update_policy(), Some(UpdatePolicy::Always));

        let cli = parse(&["jv", "tree", "--no-update"]);
        let Command::Tree(tree) = cli.command else {
            panic!("expected tree")
        };
        assert_eq!(tree.common.update_policy(), Some(UpdatePolicy::Never));

        let cli = parse(&["jv", "tree"]);
        let Command::Tree(tree) = cli.command else {
            panic!("expected tree")
        };
        // Neither flag means "whatever the repository's policy says".
        assert_eq!(tree.common.update_policy(), None);
    }

    #[test]
    fn asking_for_updates_and_no_updates_at_once_is_refused() {
        assert!(Cli::try_parse_from(["jv", "tree", "-U", "--no-update"]).is_err());
    }

    #[test]
    fn an_unknown_output_type_is_refused() {
        // Maven silently falls back to text here; saying so is more useful than
        // rendering the wrong format.
        assert!(Cli::try_parse_from(["jv", "tree", "--output-type", "yaml"]).is_err());
    }
}
