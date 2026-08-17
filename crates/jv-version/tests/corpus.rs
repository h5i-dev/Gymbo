//! Corpus-driven conformance tests.
//!
//! The files under `tests/corpus/` are transcriptions of the assertions made by
//! the upstream Java test suites (Maven Resolver's `generic` scheme, plus
//! Maven's legacy `maven-artifact` implementation). Running them here is how jv
//! claims Maven-compatible version semantics rather than merely plausible ones.
//!
//! The two upstream implementations do not agree everywhere, and jv follows
//! Maven Resolver, because that is what Maven uses at runtime. The corpus
//! isolates the disagreements in clearly marked sections; this harness skips
//! them and reports how many it skipped, so a corpus that silently loses its
//! markers fails loudly instead of quietly asserting the wrong semantics.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jv_version::{Constraint, Range, Version, qualifier};

/// Section headers containing any of these are describing semantics jv
/// deliberately does not implement, or assertions with no directive form.
const DIVERGENT_SECTION_MARKERS: &[&str] = &[
    "maven-artifact-only",
    "contradict",
    "unexpressible",
    "programmatic",
    "extension",
];

/// A comment carrying this marks the single directive that follows as a known
/// disagreement between the two upstream implementations.
const DISAGREEMENT_MARKER: &str = "DISAGREEMENT?";

struct Directive {
    line_no: usize,
    source: String,
    keyword: String,
    /// Everything after the keyword, with leading whitespace removed. Used by
    /// directives whose payload is not whitespace-delimited.
    rest: String,
    /// The payload split into fields: on TAB when the line contains one (so
    /// specs may hold spaces), otherwise on whitespace.
    fields: Vec<String>,
}

impl Directive {
    fn field(&self, i: usize) -> Result<&str, String> {
        self.fields
            .get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("expected at least {} fields", i + 1))
    }
}

struct Corpus {
    directives: Vec<Directive>,
    skipped: usize,
}

fn corpus_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
        .join(name)
}

fn read_corpus(name: &str) -> Corpus {
    let path = corpus_path(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read corpus {}: {e}", path.display()));

    let mut directives = Vec::new();
    let mut skipped = 0usize;
    let mut active = true;
    let mut source = String::from("<no source>");
    let mut pending_disagreement = false;

    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            // Decorative rules are all '='; only a header with words in it
            // actually opens a section.
            if let Some(header) = comment.strip_prefix("===") {
                if header.chars().any(char::is_alphabetic) {
                    let lowered = header.to_lowercase();
                    active = !DIVERGENT_SECTION_MARKERS
                        .iter()
                        .any(|marker| lowered.contains(marker));
                }
            } else if let Some(src) = comment.strip_prefix("source:") {
                source = src.trim().to_owned();
            } else if comment.starts_with(DISAGREEMENT_MARKER) {
                // Must be the start of the comment: the corpus header describes
                // this marker in prose, and matching that would silently skip
                // the first real directive of the file.
                pending_disagreement = true;
            }
            continue;
        }

        if !active || pending_disagreement {
            pending_disagreement = false;
            skipped += 1;
            continue;
        }

        let (keyword, rest) = match line.split_once(|c: char| c.is_whitespace()) {
            Some((kw, rest)) => (kw.to_owned(), rest.trim_start().to_owned()),
            None => (line.to_owned(), String::new()),
        };
        let fields = if rest.contains('\t') {
            rest.split('\t')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect()
        } else {
            rest.split_whitespace().map(str::to_owned).collect()
        };

        directives.push(Directive {
            line_no,
            source: source.clone(),
            keyword,
            rest,
            fields,
        });
    }

    Corpus {
        directives,
        skipped,
    }
}

/// Runs `eval` over every active directive, reporting all failures at once.
///
/// Reporting in bulk matters: a semantics bug in the comparison core trips
/// dozens of cases, and the pattern across them is what identifies the bug.
fn run(
    name: &str,
    min_directives: usize,
    min_skipped: usize,
    eval: impl Fn(&Directive) -> Result<(), String>,
) {
    let corpus = read_corpus(name);

    assert!(
        corpus.directives.len() >= min_directives,
        "{name}: only {} active directives, expected at least {min_directives} \
         — has the corpus lost content or its section markers?",
        corpus.directives.len()
    );
    // Corpora with known divergent sections must still be seen to skip them; if
    // a marker is dropped in an edit, the harness would otherwise start
    // asserting semantics jv deliberately rejects.
    assert!(
        corpus.skipped >= min_skipped,
        "{name}: skipped only {} directives, expected at least {min_skipped} \
         — check the corpus section markers",
        corpus.skipped
    );

    let mut failures = String::new();
    let mut failed = 0usize;
    for directive in &corpus.directives {
        if let Err(message) = eval(directive) {
            failed += 1;
            if failed <= 40 {
                let _ = writeln!(
                    failures,
                    "  {name}:{} [{}] {} {}\n      {message}",
                    directive.line_no, directive.source, directive.keyword, directive.rest
                );
            }
        }
    }

    if failed > 0 {
        panic!(
            "{name}: {failed} of {} directives failed \
             (skipped {} divergent):\n{failures}{}",
            corpus.directives.len(),
            corpus.skipped,
            if failed > 40 {
                format!("  ... and {} more\n", failed - 40)
            } else {
                String::new()
            }
        );
    }

    eprintln!(
        "{name}: {} directives passed, {} divergent skipped",
        corpus.directives.len(),
        corpus.skipped
    );
}

fn expect_lt(lo: &str, hi: &str) -> Result<(), String> {
    let a = Version::parse(lo);
    let b = Version::parse(hi);
    match a.cmp(&b) {
        Ordering::Less => {}
        other => return Err(format!("{lo:?} vs {hi:?}: expected Less, got {other:?}")),
    }
    // Antisymmetry is cheap to check here and catches comparison bugs that a
    // one-directional assertion would let through.
    match b.cmp(&a) {
        Ordering::Greater => Ok(()),
        other => Err(format!(
            "{hi:?} vs {lo:?}: expected Greater (antisymmetry), got {other:?}"
        )),
    }
}

fn expect_eq(a_text: &str, b_text: &str) -> Result<(), String> {
    let a = Version::parse(a_text);
    let b = Version::parse(b_text);
    if a.cmp(&b) != Ordering::Equal {
        return Err(format!(
            "{a_text:?} vs {b_text:?}: expected Equal, got {:?}",
            a.cmp(&b)
        ));
    }
    if b.cmp(&a) != Ordering::Equal {
        return Err(format!(
            "{b_text:?} vs {a_text:?}: expected Equal in reverse"
        ));
    }
    // Equal versions must hash alike, or they break every HashMap keyed by
    // version in the resolver.
    if hash_of(&a) != hash_of(&b) {
        return Err(format!(
            "{a_text:?} and {b_text:?} compare equal but hash differently"
        ));
    }
    Ok(())
}

fn hash_of(v: &Version) -> u64 {
    use std::hash::{BuildHasher, RandomState};
    // A fixed builder so both sides hash under the same keys.
    static STATE: std::sync::OnceLock<RandomState> = std::sync::OnceLock::new();
    STATE.get_or_init(RandomState::new).hash_one(v)
}

#[test]
fn ordering_corpus() {
    run("ordering.txt", 320, 2, |d| match d.keyword.as_str() {
        "order" => {
            let items: Vec<&str> = d.rest.split('<').map(str::trim).collect();
            if items.len() < 2 {
                return Err("expected at least two versions in an order chain".into());
            }
            for i in 0..items.len() {
                for j in i + 1..items.len() {
                    expect_lt(items[i], items[j])?;
                }
            }
            Ok(())
        }
        "eq" => {
            let items: Vec<&str> = d.rest.split("==").map(str::trim).collect();
            if items.len() < 2 {
                return Err("expected at least two versions in an eq group".into());
            }
            for i in 0..items.len() {
                for j in i + 1..items.len() {
                    expect_eq(items[i], items[j])?;
                }
            }
            Ok(())
        }
        // Pipe-separated forms exist so payloads may contain spaces or be empty.
        "lt" | "eqp" => {
            let (a, b) = d
                .rest
                .split_once('|')
                .ok_or("expected two pipe-separated versions")?;
            if d.keyword == "lt" {
                expect_lt(a, b)
            } else {
                expect_eq(a, b)
            }
        }
        other => Err(format!("unknown directive {other:?}")),
    });
}

#[test]
fn ranges_corpus() {
    run("ranges.txt", 50, 30, |d| {
        let spec = d.field(0)?;
        match d.keyword.as_str() {
            "contains" | "excludes" => {
                let version = Version::parse(d.field(1)?);
                let range = Range::parse(spec).map_err(|e| format!("parse failed: {e}"))?;
                let want = d.keyword == "contains";
                if range.contains(&version) == want {
                    Ok(())
                } else {
                    Err(format!(
                        "{spec} should {} {version}",
                        if want { "contain" } else { "exclude" }
                    ))
                }
            }
            "invalid" => match Range::parse(spec) {
                Err(_) => Ok(()),
                Ok(range) => Err(format!("expected a parse error, got {range}")),
            },
            "lower" | "upper" => {
                let range = Range::parse(spec).map_err(|e| format!("parse failed: {e}"))?;
                let bound = if d.keyword == "lower" {
                    range.lower_bound()
                } else {
                    range.upper_bound()
                };
                check_bound(bound, d)
            }
            other => Err(format!("unknown directive {other:?}")),
        }
    });
}

#[test]
fn constraints_corpus() {
    run("constraints.txt", 30, 4, |d| {
        let spec = d.field(0)?;
        match d.keyword.as_str() {
            "contains" | "excludes" => {
                let version = Version::parse(d.field(1)?);
                let constraint =
                    Constraint::parse(spec).map_err(|e| format!("parse failed: {e}"))?;
                let want = d.keyword == "contains";
                if constraint.contains(&version) == want {
                    Ok(())
                } else {
                    Err(format!(
                        "{spec} should {} {version}",
                        if want { "contain" } else { "exclude" }
                    ))
                }
            }
            "invalid" => match Constraint::parse(spec) {
                Err(_) => Ok(()),
                Ok(c) => Err(format!("expected a parse error, got {c}")),
            },
            "kind" => {
                let constraint =
                    Constraint::parse(spec).map_err(|e| format!("parse failed: {e}"))?;
                match d.field(1)? {
                    "version" => {
                        if constraint.version().is_some() {
                            Ok(())
                        } else {
                            Err(format!("{spec} should parse to a bare version"))
                        }
                    }
                    "range" => {
                        if constraint.range_set().is_some() {
                            Ok(())
                        } else {
                            Err(format!("{spec} should parse to a range set"))
                        }
                    }
                    other => Err(format!("unknown kind {other:?}")),
                }
            }
            "lower" | "upper" => {
                let constraint =
                    Constraint::parse(spec).map_err(|e| format!("parse failed: {e}"))?;
                let bound = if d.keyword == "lower" {
                    constraint.lower_bound()
                } else {
                    constraint.upper_bound()
                };
                check_bound(bound, d)
            }
            other => Err(format!("unknown directive {other:?}")),
        }
    });
}

#[test]
fn qualifiers_corpus() {
    run("qualifiers.txt", 10, 0, |d| {
        if d.keyword != "qualifier" {
            return Err(format!("unknown directive {:?}", d.keyword));
        }
        let input = d.field(0)?;
        let expected = d.field(1)?;
        let actual = qualifier(input);
        let want = if expected == "none" {
            None
        } else {
            Some(
                expected
                    .parse::<i32>()
                    .map_err(|_| format!("bad shift {expected:?}"))?,
            )
        };
        if actual == want {
            Ok(())
        } else {
            Err(format!("{input:?}: expected {want:?}, got {actual:?}"))
        }
    });
}

/// Shared checker for `lower`/`upper` directives over either a range or a
/// constraint: `<spec> none` or `<spec> <version> inclusive|exclusive`.
fn check_bound(bound: Option<&jv_version::Bound>, d: &Directive) -> Result<(), String> {
    let expected = d.field(1)?;
    if expected == "none" {
        return match bound {
            None => Ok(()),
            Some(b) => Err(format!("expected no bound, got {}", b.version())),
        };
    }
    let bound = bound.ok_or_else(|| format!("expected bound {expected}, got none"))?;
    let want_version = Version::parse(expected);
    if bound.version() != &want_version {
        return Err(format!(
            "expected bound version {expected}, got {}",
            bound.version()
        ));
    }
    let inclusive = match d.field(2)? {
        "inclusive" => true,
        "exclusive" => false,
        other => return Err(format!("unknown inclusivity {other:?}")),
    };
    if bound.is_inclusive() == inclusive {
        Ok(())
    } else {
        Err(format!(
            "expected {} bound, got {}",
            d.field(2)?,
            if bound.is_inclusive() {
                "inclusive"
            } else {
                "exclusive"
            }
        ))
    }
}
