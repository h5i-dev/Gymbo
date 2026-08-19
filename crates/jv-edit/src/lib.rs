//! Editing a POM without reformatting it.
//!
//! # Why this is not "parse, modify, serialise"
//!
//! A POM is a file a human wrote and a team reviews. Round-tripping it through
//! a document model rewrites attribute order, collapses or expands whitespace,
//! reindents, and drops comments — so `jv add` would produce a diff touching
//! every line of a file it changed one dependency in. A tool that does that
//! gets removed from the project, whatever else it does well.
//!
//! So the model is never serialised back. The source text is scanned for the
//! byte range of the element to change, and only that range is rewritten;
//! everything outside it is copied through unchanged, byte for byte. Comments,
//! CDATA, the XML declaration, CRLF line endings, tabs, and whatever the author
//! did with blank lines all survive because nothing here looks at them.
//!
//! # What this crate decides, and what it does not
//!
//! It decides *where* text goes and *what it looks like*. It does not decide
//! which version to write, or whether to write one at all — that needs the
//! effective model, since a dependency managed by a parent or an imported BOM
//! must be added without a `<version>`, and getting that wrong is the fastest
//! way for a tool to lose trust. The caller supplies a finished [`Dependency`].

use quick_xml::Reader;
use quick_xml::events::Event;

/// What a POM could not be edited for.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("malformed XML: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("this is not a POM: the root element is <{found}>, not <project>")]
    NotAProject { found: String },
    #[error("no <project> element")]
    Empty,
}

/// A dependency to write, exactly as it should appear.
///
/// `version` is `None` for a dependency whose version comes from
/// `<dependencyManagement>` or an imported BOM, where writing one would pin
/// what the project deliberately left managed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dependency {
    pub group_id: String,
    pub artifact_id: String,
    pub version: Option<String>,
    pub scope: Option<String>,
    pub classifier: Option<String>,
    pub type_: Option<String>,
    pub optional: bool,
}

/// What happened, so the caller can say something true about it.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    /// The POM now contains the dependency; here is its new text.
    Inserted(String),
    /// A dependency with these coordinates is already declared, at this line.
    ///
    /// Not an error and not something to silently duplicate: Maven warns about
    /// duplicate declarations and then honours the last, which is a confusing
    /// thing for a tool to have caused.
    AlreadyPresent {
        line: usize,
        version: Option<String>,
    },
}

/// Adds a dependency to `<project><dependencies>`, creating that element if the
/// POM has none.
pub fn add_dependency(pom: &str, dependency: &Dependency) -> Result<Added, EditError> {
    let layout = scan(pom)?;

    if let Some(existing) = layout.dependencies.iter().find(|held| {
        held.group_id == dependency.group_id && held.artifact_id == dependency.artifact_id
    }) {
        return Ok(Added::AlreadyPresent {
            line: existing.line,
            version: existing.version.clone(),
        });
    }

    let indent = layout.indent_unit(pom);
    Ok(Added::Inserted(match layout.dependencies_block {
        // An existing block: the new entry goes last, indented like its
        // siblings so the file keeps whatever convention it already had.
        Some(block) => {
            let child_indent = layout
                .child_indent
                .clone()
                .unwrap_or_else(|| format!("{}{indent}", block.own_indent));
            let rendered = render(dependency, &child_indent, &indent, &layout.newline);
            let mut out = String::with_capacity(pom.len() + rendered.len());
            out.push_str(&pom[..block.insert_at]);
            out.push_str(&layout.newline);
            out.push_str(&rendered);
            if block.is_empty {
                // An empty element holds only whitespace, and whatever that
                // was is no longer the right shape once it has a child. It is
                // replaced rather than kept, so `<dependencies>\n  </dependencies>`
                // does not come out with a blank line where its content used to
                // be.
                out.push_str(&layout.newline);
                out.push_str(&block.own_indent);
                out.push_str(&pom[block.close_at..]);
            } else {
                // A block that already had children keeps everything after its
                // last one: the newline and indent before `</dependencies>`,
                // which is exactly what should follow the new entry too.
                out.push_str(&pom[block.insert_at..]);
            }
            out
        }
        // No block at all: one is created just before `</project>`, at the
        // indentation the project's other children use.
        None => {
            let own_indent = layout
                .project_child_indent
                .clone()
                .unwrap_or_else(|| indent.clone());
            let child_indent = format!("{own_indent}{indent}");
            let rendered = render(dependency, &child_indent, &indent, &layout.newline);
            let newline = &layout.newline;
            let block = format!(
                "{own_indent}<dependencies>{newline}{rendered}{newline}{own_indent}</dependencies>{newline}"
            );
            let mut out = String::with_capacity(pom.len() + block.len());
            out.push_str(&pom[..layout.project_end]);
            out.push_str(&block);
            out.push_str(&pom[layout.project_end..]);
            out
        }
    }))
}

/// What removing a dependency did.
#[derive(Debug, PartialEq, Eq)]
pub enum Removed {
    /// The POM no longer contains it; here is its new text.
    Removed(String),
    /// Nothing declared these coordinates, so nothing changed.
    NotPresent,
}

/// Removes a dependency from `<project><dependencies>`.
///
/// The element and the line it sits on go; nothing else does. In particular a
/// comment above the entry is left alone, even though it very likely described
/// it. Guessing which comments belong to which element is how an editing tool
/// deletes a note somebody needed, and a stray comment is a much cheaper
/// mistake than a lost one — `jv` says what it removed, and the reviewer can
/// see the comment in the same diff.
///
/// An emptied `<dependencies>` is left in place rather than removed, for the
/// same reason: it may contain comments, and an empty one is valid.
pub fn remove_dependency(
    pom: &str,
    group_id: &str,
    artifact_id: &str,
) -> Result<Removed, EditError> {
    let layout = scan(pom)?;
    let Some(entry) = layout
        .dependencies
        .iter()
        .find(|held| held.group_id == group_id && held.artifact_id == artifact_id)
    else {
        return Ok(Removed::NotPresent);
    };

    // Take the whole line when the element has it to itself, so removing an
    // entry does not leave the indentation that used to precede it.
    let line_begins = line_start(pom, entry.start);
    let start = if pom[line_begins..entry.start].trim().is_empty() {
        line_begins
    } else {
        entry.start
    };

    let mut end = entry.end;
    let rest = &pom[end..];
    let line_ends = rest.find('\n').map_or(rest.len(), |index| index + 1);
    if rest[..line_ends].trim().is_empty() {
        end += line_ends;
    }

    let mut out = String::with_capacity(pom.len());
    out.push_str(&pom[..start]);
    out.push_str(&pom[end..]);
    Ok(Removed::Removed(out))
}

/// Renders one `<dependency>` element.
fn render(dependency: &Dependency, indent: &str, unit: &str, newline: &str) -> String {
    let inner = format!("{indent}{unit}");
    let mut out = format!("{indent}<dependency>{newline}");
    let mut field = |name: &str, value: &str| {
        out.push_str(&format!(
            "{inner}<{name}>{}</{name}>{newline}",
            escape(value)
        ));
    };
    field("groupId", &dependency.group_id);
    field("artifactId", &dependency.artifact_id);
    if let Some(version) = &dependency.version {
        field("version", version);
    }
    if let Some(classifier) = &dependency.classifier {
        field("classifier", classifier);
    }
    if let Some(type_) = &dependency.type_ {
        field("type", type_);
    }
    if let Some(scope) = &dependency.scope {
        field("scope", scope);
    }
    if dependency.optional {
        field("optional", "true");
    }
    out.push_str(&format!("{indent}</dependency>"));
    out
}

/// Escapes the five characters that cannot appear literally in element content.
///
/// Coordinates never need this, but a version can be a property expression and
/// nothing stops a caller passing something odder; producing a POM that no
/// longer parses would be a worse failure than an ugly one.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out
}

/// A `<dependency>` already declared, and where.
#[derive(Debug)]
struct Existing {
    group_id: String,
    artifact_id: String,
    version: Option<String>,
    line: usize,
    /// Byte offset of `<dependency>`.
    start: usize,
    /// Byte offset just past `</dependency>`.
    end: usize,
}

/// Where the project's `<dependencies>` sits in the text.
#[derive(Debug)]
struct Block {
    /// Byte offset to insert a new child at: just before the closing tag's
    /// leading whitespace.
    insert_at: usize,
    /// Indentation of the `<dependencies>` tag itself.
    own_indent: String,
    /// Whether the element has no `<dependency>` children.
    is_empty: bool,
    /// Byte offset of the closing `</dependencies>` tag.
    close_at: usize,
}

#[derive(Debug, Default)]
struct Layout {
    dependencies_block: Option<Block>,
    dependencies: Vec<Existing>,
    /// Indentation shared by existing `<dependency>` children.
    child_indent: Option<String>,
    /// Indentation of `<project>`'s own children, for creating a new block.
    project_child_indent: Option<String>,
    /// Byte offset of `</project>`'s line start.
    project_end: usize,
    newline: String,
}

impl Layout {
    /// One level of indentation, as this file spells it.
    ///
    /// Taken from the project's own children rather than assumed, so a
    /// four-space or tab-indented POM stays that way.
    fn indent_unit(&self, pom: &str) -> String {
        if let Some(indent) = &self.project_child_indent
            && !indent.is_empty()
        {
            return indent.clone();
        }
        if pom.contains("\n\t") {
            "\t".to_owned()
        } else {
            "  ".to_owned()
        }
    }
}

/// Finds everything the edit needs, in one pass over the source.
fn scan(pom: &str) -> Result<Layout, EditError> {
    let mut reader = Reader::from_str(pom);
    reader.config_mut().check_end_names = false;

    let mut layout = Layout {
        newline: if pom.contains("\r\n") { "\r\n" } else { "\n" }.to_owned(),
        project_end: pom.len(),
        ..Layout::default()
    };

    let mut path: Vec<String> = Vec::new();
    let mut root_seen = false;
    // The `<dependency>` currently being read, when inside the project's own
    // `<dependencies>`.
    let mut current: Option<Existing> = None;
    let mut text = String::new();

    loop {
        let before = reader.buffer_position() as usize;
        let event = reader.read_event()?;
        let after = reader.buffer_position() as usize;

        match event {
            Event::Start(element) => {
                let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
                if !root_seen {
                    root_seen = true;
                    if name != "project" {
                        return Err(EditError::NotAProject { found: name });
                    }
                }
                path.push(name.clone());

                if path == ["project", "dependencies"] {
                    layout.dependencies_block = Some(Block {
                        insert_at: after,
                        own_indent: leading_whitespace(pom, before),
                        is_empty: true,
                        close_at: 0,
                    });
                } else if path == ["project", "dependencies", "dependency"] {
                    current = Some(Existing {
                        group_id: String::new(),
                        artifact_id: String::new(),
                        version: None,
                        line: line_of(pom, before),
                        start: before,
                        end: before,
                    });
                    if layout.child_indent.is_none() {
                        layout.child_indent = Some(leading_whitespace(pom, before));
                    }
                } else if path.len() == 2 && layout.project_child_indent.is_none() {
                    layout.project_child_indent = Some(leading_whitespace(pom, before));
                }
                text.clear();
            }
            Event::Text(chunk) => {
                text.push_str(&chunk.unescape().unwrap_or_default());
            }
            Event::End(_) => {
                let closed = path.pop();
                if let Some(closed) = closed.as_deref() {
                    if path == ["project", "dependencies", "dependency"]
                        && let Some(entry) = current.as_mut()
                    {
                        let value = text.trim().to_owned();
                        match closed {
                            "groupId" => entry.group_id = value,
                            "artifactId" => entry.artifact_id = value,
                            "version" => entry.version = Some(value),
                            _ => {}
                        }
                    } else if path == ["project", "dependencies"] && closed == "dependency" {
                        if let Some(mut entry) = current.take() {
                            entry.end = after;
                            layout.dependencies.push(entry);
                        }
                        // A child closed, so the block is not empty, and the
                        // insertion point moves to just after it.
                        if let Some(block) = layout.dependencies_block.as_mut() {
                            block.is_empty = false;
                            block.insert_at = after;
                        }
                    } else if path == ["project"] && closed == "dependencies" {
                        if let Some(block) = layout.dependencies_block.as_mut() {
                            block.close_at = before;
                        }
                    } else if path.is_empty() && closed == "project" {
                        layout.project_end = line_start(pom, before);
                    }
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !root_seen {
        return Err(EditError::Empty);
    }
    Ok(layout)
}

/// The whitespace at the start of the line `offset` falls on.
fn leading_whitespace(pom: &str, offset: usize) -> String {
    let start = line_start(pom, offset);
    pom[start..offset.min(pom.len())]
        .chars()
        .take_while(|character| *character == ' ' || *character == '\t')
        .collect()
}

/// The byte offset of the start of the line `offset` falls on.
fn line_start(pom: &str, offset: usize) -> usize {
    let offset = offset.min(pom.len());
    pom[..offset].rfind('\n').map_or(0, |index| index + 1)
}

/// The 1-based line number `offset` falls on.
fn line_of(pom: &str, offset: usize) -> usize {
    pom[..offset.min(pom.len())].matches('\n').count() + 1
}
