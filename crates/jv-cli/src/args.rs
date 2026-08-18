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
    /// Run a JVM tool straight from its Maven coordinates.
    Exec(ExecArgs),
    /// Download everything the build needs, so `mvn -o` works afterwards.
    Sync(SyncArgs),
    /// Report where a Maven build spends its time.
    Profile(ProfileArgs),
    /// Add a dependency to a POM.
    Add(AddArgs),
    /// Remove a dependency from a POM.
    Remove(RemoveArgs),
    /// Report dependencies with newer versions available.
    Outdated(OutdatedArgs),
}

#[derive(Args, Debug)]
pub struct OutdatedArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// The pom.xml to read. Defaults to the nearest one at or above the working
    /// directory.
    #[arg(short = 'f', long = "file", value_name = "POM")]
    pub file: Option<PathBuf>,

    /// Check every module of a multi-module build.
    #[arg(long, default_value_t = true)]
    pub recursive: bool,

    /// Only this module, even in a multi-module build.
    #[arg(long, conflicts_with = "recursive")]
    pub no_recursive: bool,

    /// Check plugins as well as dependencies.
    #[arg(long)]
    pub plugins: bool,

    /// Consider alpha, beta, milestone and release-candidate versions.
    ///
    /// Off by default: a project pinned to a stable release is not "outdated"
    /// because a release candidate exists, and a tool that says otherwise gets
    /// ignored.
    #[arg(long)]
    pub pre_release: bool,

    /// Exit non-zero when anything is outdated, for use as a CI gate.
    #[arg(long)]
    pub exit_code: bool,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// The dependency, as `group:artifact`. A version is accepted and ignored,
    /// so a line copied from `jv add` works.
    #[arg(value_name = "COORDINATES")]
    pub coordinates: String,

    /// The pom.xml to edit. Defaults to the nearest one at or above the working
    /// directory.
    #[arg(short = 'f', long = "file", value_name = "POM")]
    pub file: Option<PathBuf>,

    /// Remove from this module of a multi-module build, by artifact id.
    #[arg(short = 'm', long, value_name = "MODULE")]
    pub module: Option<String>,

    /// Print the edited POM instead of writing it.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// The dependency, as `group:artifact` or `group:artifact:version`.
    ///
    /// With no version, jv writes none if the project already manages one —
    /// through `<dependencyManagement>` or an imported BOM — and otherwise
    /// resolves the newest release from the repository.
    #[arg(value_name = "COORDINATES")]
    pub coordinates: String,

    /// The pom.xml to edit. Defaults to the nearest one at or above the working
    /// directory.
    #[arg(short = 'f', long = "file", value_name = "POM")]
    pub file: Option<PathBuf>,

    /// Add to this module of a multi-module build, by artifact id.
    #[arg(short = 'm', long, value_name = "MODULE")]
    pub module: Option<String>,

    /// `<scope>` to declare. `--test` is shorthand for `--scope test`.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Shorthand for `--scope test`.
    #[arg(long, conflicts_with = "scope")]
    pub test: bool,

    /// `<classifier>` to declare.
    #[arg(long, value_name = "CLASSIFIER")]
    pub classifier: Option<String>,

    /// `<type>` to declare, such as `pom` or `test-jar`.
    #[arg(long = "type", value_name = "TYPE")]
    pub type_: Option<String>,

    /// Mark the dependency `<optional>`.
    #[arg(long)]
    pub optional: bool,

    /// Print the edited POM instead of writing it.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct ProfileArgs {
    /// The `EventSpy` jar to load.
    ///
    /// Defaults to `jv-profiler.jar` beside the `jv` executable, then to a
    /// build tree's `java/jv-profiler/target/`. `JV_PROFILER_JAR` overrides
    /// both.
    #[arg(long, value_name = "JAR")]
    pub profiler_jar: Option<PathBuf>,

    /// The command to measure, after `--`. Defaults to `mvn test`.
    ///
    /// Anything is accepted rather than only `mvn`, because the wrappers people
    /// actually run — `./mvnw`, `mvnd` — take the same property.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// The pom.xml to read. Defaults to the nearest one at or above the working
    /// directory.
    #[arg(short = 'f', long = "file", value_name = "POM")]
    pub file: Option<PathBuf>,

    /// Sync every module of a multi-module build.
    #[arg(long, default_value_t = true)]
    pub recursive: bool,

    /// Only this module, even in a multi-module build.
    #[arg(long, conflicts_with = "recursive")]
    pub no_recursive: bool,

    /// Where to put the artifacts. Defaults to Maven's local repository, which
    /// is what makes `mvn -o` work.
    #[arg(long, value_name = "DIR")]
    pub local_repository: Option<PathBuf>,

    /// Also sync these artifacts, as `group:artifact:version`.
    ///
    /// For what no POM reveals. Some plugins resolve a default of their own at
    /// execution time, from a version compiled into the plugin rather than
    /// written down anywhere: `spotless` picks a formatter that way, and which
    /// one depends on the spotless release — 2.40.0 wants
    /// palantir-java-format 2.38.0, 2.43.0 wants 2.39.0. jv reads what a POM
    /// declares, including the coordinates inside plugin `<configuration>`, but
    /// a version that exists only inside a jar cannot be read out of a project.
    ///
    /// So this is the escape hatch, rather than jv carrying a table of every
    /// plugin's defaults that would be wrong the next time one of them is
    /// released. Take the coordinates from what `mvn` says is missing.
    #[arg(long = "also", value_name = "COORDINATES")]
    pub also: Vec<String>,

    /// Fill jv's own cache but do not touch Maven's local repository.
    #[arg(long, conflicts_with = "local_repository")]
    pub cache_only: bool,

    /// Do not resolve the project's plugins.
    ///
    /// Faster, and enough for jv's own commands — but a repository synced this
    /// way cannot run `mvn -o`, which is the usual reason to sync at all.
    #[arg(long)]
    pub no_plugins: bool,

    /// Also resolve the dependencies of `<pluginManagement>` plugins that no
    /// `<plugins>` block declares.
    ///
    /// Off by default. Management gives a version and configuration to plugins
    /// that are declared; an entry nothing declares never enters a build plan,
    /// so Maven never loads its dependencies. Skipping them cut spring-petclinic
    /// from 345 MB to a third of that. Turn this on to invoke a
    /// management-only plugin directly, as `mvn -o some:goal`.
    #[arg(long)]
    pub all_plugins: bool,
}

/// The `jvx` binary: `jv exec` with the subcommand implied.
#[derive(Debug, Parser)]
#[command(name = "jvx", version, about = "Run a JVM tool straight from its Maven coordinates.", long_about = None)]
pub struct Jvx {
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// Options every command that resolves shares.
#[derive(Args, Debug, Clone)]
pub struct CommonArgs {
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

    /// Contact repositories over plaintext HTTP.
    ///
    /// Refused by default, as Maven 3.8 and later refuse it: an artifact fetched
    /// over http:// can be replaced in transit, and its checksum travels the same
    /// wire. localhost is always allowed.
    #[arg(long)]
    pub allow_insecure_http: bool,

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

    /// The pom.xml to read. Defaults to the nearest one at or above the working
    /// directory.
    #[arg(short = 'f', long = "file", value_name = "POM")]
    pub file: Option<PathBuf>,

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

    /// The pom.xml to read. Defaults to the nearest one at or above the working
    /// directory.
    #[arg(short = 'f', long = "file", value_name = "POM")]
    pub file: Option<PathBuf>,

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

#[derive(Args, Debug)]
pub struct ExecArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// The class to run, overriding the jar's `Main-Class`.
    ///
    /// Also makes the endpoint's fields strictly positional, which is how a
    /// classifier that looks like a class name is spelled.
    #[arg(long = "main", value_name = "CLASS")]
    pub main: Option<String>,

    /// What to run: `group:artifact[:version[:classifier]][@mainClass]`.
    #[arg(value_name = "ENDPOINT")]
    pub endpoint: String,

    /// Arguments for the tool, passed through untouched.
    ///
    /// Put `--` before any argument that starts with a dash, as `cargo run` and
    /// `npx` do.
    // That instruction is not a style preference. clap only starts reading this
    // positional raw once it has taken a value, so `jvx tool --version` is the
    // one arrangement where the flag is still jvx's, and no combination of clap
    // settings fixes it while jvx has flags of its own. `--` always works.
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub args: Vec<String>,
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

    fn jvx(args: &[&str]) -> ExecArgs {
        Jvx::try_parse_from(args).expect("valid arguments").exec
    }

    #[test]
    fn the_jvx_command_definition_is_valid() {
        Jvx::command().debug_assert();
    }

    #[test]
    fn everything_after_a_double_dash_reaches_the_tool() {
        let args = jvx(&["jvx", "g:a", "--", "--version"]);
        assert_eq!(args.endpoint, "g:a");
        // The `--` itself is jvx's separator and must not be forwarded, or every
        // tool would see an argument it never expected.
        assert_eq!(args.args, ["--version"]);
    }

    #[test]
    fn a_plain_tool_argument_needs_no_double_dash() {
        // Once the trailing positional has a value, everything after it is raw,
        // flags included.
        let args = jvx(&["jvx", "g:a", "Main.java", "--dry-run"]);
        assert_eq!(args.args, ["Main.java", "--dry-run"]);
    }

    #[test]
    fn a_flag_immediately_after_the_endpoint_is_still_jvxs() {
        // Documented rather than fixed: clap has not entered raw mode yet, so
        // `--version` here is jvx's own. The failure is loud, and `--` is the
        // spelling that always reaches the tool.
        assert!(Jvx::try_parse_from(["jvx", "g:a", "--version"]).is_err());
    }

    #[test]
    fn jvx_options_are_read_before_the_endpoint() {
        let args = jvx(&[
            "jvx",
            "-o",
            "--main",
            "com.example.Other",
            "g:a:1.0",
            "--",
            "x",
        ]);
        assert!(args.common.offline);
        assert_eq!(args.main.as_deref(), Some("com.example.Other"));
        assert_eq!(args.endpoint, "g:a:1.0");
        assert_eq!(args.args, ["x"]);
    }

    #[test]
    fn a_double_dash_inside_the_tool_arguments_survives() {
        // Only the first `--` is jvx's; a second one belongs to whatever the
        // tool does with it.
        let args = jvx(&["jvx", "g:a", "--", "run", "--", "nested"]);
        assert_eq!(args.args, ["run", "--", "nested"]);
    }

    #[test]
    fn jv_exec_takes_the_same_arguments_as_jvx() {
        let cli = parse(&["jv", "exec", "g:a:1.0", "--", "--version"]);
        let Command::Exec(exec) = cli.command else {
            panic!("expected exec")
        };
        assert_eq!(exec.endpoint, "g:a:1.0");
        assert_eq!(exec.args, ["--version"]);
    }

    #[test]
    fn jvx_needs_an_endpoint() {
        assert!(Jvx::try_parse_from(["jvx"]).is_err());
    }

    #[test]
    fn an_unknown_output_type_is_refused() {
        // Maven silently falls back to text here; saying so is more useful than
        // rendering the wrong format.
        assert!(Cli::try_parse_from(["jv", "tree", "--output-type", "yaml"]).is_err());
    }
}
