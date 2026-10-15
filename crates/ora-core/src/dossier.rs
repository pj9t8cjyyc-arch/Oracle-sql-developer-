//! Markdown "dossiers" from the knowledge base: for one object and its neighbourhood in the
//! graph, one file per object with its facts, the SQL/PL-SQL that matters, and every link with
//! its evidence and certainty. Meant to be read by people and to be fed to a model.
//!
//! Row data is never included. Source text goes through [`crate::sqltext::redact_for_export`].

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::catalog;
use crate::db::Database;
use crate::graph::{Direction, Focus, GNode, Subgraph};
use crate::knowledge::{Knowledge, TableFacts};
use crate::packages::PackageDetail;
use crate::sqltext::redact_for_export;
use crate::{Error, Result};

/// Where long text (source, DDL) comes from.
pub trait Fetch {
    fn source(&self, owner: &str, name: &str, kind: &str) -> Result<String>;
    fn ddl(&self, owner: &str, name: &str, kind: &str) -> Result<String>;
}

pub struct DbFetch<'a>(pub &'a dyn Database);

impl Fetch for DbFetch<'_> {
    fn source(&self, owner: &str, name: &str, kind: &str) -> Result<String> {
        catalog::object_source(self.0, owner, name, kind)
    }
    fn ddl(&self, owner: &str, name: &str, kind: &str) -> Result<String> {
        catalog::object_ddl(self.0, owner, name, kind)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DossierOptions {
    /// Node id such as `TABLE:HR.EMPLOYEES`.
    pub focus: String,
    pub direction: Direction,
    pub hops: u32,
    /// Include source code and DDL (otherwise facts and links only).
    pub include_source: bool,
    /// Cap on lines of code shown per package member (0 = no cap).
    pub max_member_lines: usize,
    pub max_nodes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bundle {
    pub files: Vec<BundleFile>,
    /// Rough size for a language model (characters / 4).
    pub tokens_estimate: usize,
    /// The neighbourhood was cut at `max_nodes`.
    pub truncated: bool,
    /// Objects whose source or DDL could not be read.
    pub missing: Vec<String>,
}

impl Bundle {
    /// One Markdown document with every file in order.
    pub fn combined(&self) -> String {
        let mut out = String::new();
        for f in &self.files {
            if !f.path.ends_with(".md") {
                continue;
            }
            let _ = write!(
                out,
                "<!-- file: {} -->\n{}\n\n",
                f.path,
                f.content.trim_end()
            );
        }
        out
    }

    pub fn write_combined(&self, path: &Path) -> Result<()> {
        write_atomic(path, self.combined().as_bytes())
    }

    pub fn write_zip(&self, path: &Path) -> Result<()> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for f in &self.files {
                z.start_file(&f.path, opts)
                    .map_err(|e| Error::Invalid(format!("zip: {e}")))?;
                z.write_all(f.content.as_bytes()).map_err(io)?;
            }
            z.finish()
                .map_err(|e| Error::Invalid(format!("zip: {e}")))?;
        }
        write_atomic(path, &buf.into_inner())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })
}

fn io(e: std::io::Error) -> Error {
    Error::Invalid(format!("cannot write the file: {e}"))
}

fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '#' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn file_for(n: &GNode) -> String {
    format!(
        "objects/{}/{}.{}.md",
        n.kind.to_lowercase().replace(' ', "_"),
        safe(&n.owner),
        safe(&n.name)
    )
}

fn code(lang: &str, text: &str) -> String {
    let fence = if text.contains("```") { "~~~~" } else { "```" };
    format!("{fence}{lang}\n{}\n{fence}\n", text.trim_end())
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut o = format!(
        "| {} |\n|{}|\n",
        headers.join(" | "),
        headers.iter().map(|_| "---").collect::<Vec<_>>().join("|")
    );
    for r in rows {
        let _ = writeln!(
            o,
            "| {} |",
            r.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | ")
        );
    }
    o
}

struct Ctx<'a> {
    kb: &'a Knowledge,
    fetch: &'a dyn Fetch,
    opts: &'a DossierOptions,
    sub: &'a Subgraph,
    ids: BTreeSet<&'a str>,
    cache: RefCell<HashMap<(String, String), std::result::Result<String, String>>>,
    missing: RefCell<BTreeSet<String>>,
}

impl<'a> Ctx<'a> {
    fn text(&self, is_ddl: bool, owner: &str, name: &str, kind: &str) -> Option<String> {
        if !self.opts.include_source {
            return None;
        }
        let key = (
            format!("{}{kind}", if is_ddl { "ddl:" } else { "src:" }),
            format!("{owner}.{name}"),
        );
        let hit = self.cache.borrow().get(&key).cloned();
        let r = hit.unwrap_or_else(|| {
            let r = if is_ddl {
                self.fetch.ddl(owner, name, kind)
            } else {
                self.fetch.source(owner, name, kind)
            }
            .map(|t| redact_for_export(&t))
            .map_err(|e| e.to_string());
            self.cache.borrow_mut().insert(key.clone(), r.clone());
            r
        });
        match r {
            Ok(t) if !t.trim().is_empty() => Some(t),
            Ok(_) => {
                self.missing.borrow_mut().insert(format!(
                    "{kind} {owner}.{name}: no readable text (wrapped or not visible)"
                ));
                None
            }
            Err(e) => {
                self.missing
                    .borrow_mut()
                    .insert(format!("{kind} {owner}.{name}: {e}"));
                None
            }
        }
    }

    fn in_view(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    fn link(&self, id: &str) -> String {
        match self.kb.nodes.iter().find(|n| n.id == id) {
            Some(n) if self.in_view(id) && has_file(&n.kind) => {
                format!(
                    "[{}.{}]({})",
                    n.owner,
                    n.name,
                    rel(&n.kind, &n.owner, &n.name)
                )
            }
            Some(n) => format!("{}.{}", n.owner, n.name),
            None => id.to_string(),
        }
    }

    /// Edges of the knowledge graph touching `id`, with the other end.
    fn edges_of(&self, id: &str) -> (Vec<&'a crate::graph::GEdge>, Vec<&'a crate::graph::GEdge>) {
        let outgoing = self.kb.edges.iter().filter(|e| e.from == id).collect();
        let incoming = self.kb.edges.iter().filter(|e| e.to == id).collect();
        (outgoing, incoming)
    }

    fn evidence(&self, a: &str, b: &str) -> String {
        let ev = self.kb.evidence_between(a, b);
        if ev.is_empty() {
            return String::new();
        }
        ev.iter()
            .map(|e| format!("{} [{}, {}]", e.detail, e.source, e.confidence))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn node(&self, id: &str) -> Option<&'a GNode> {
        self.kb.nodes.iter().find(|n| n.id == id)
    }
}

fn has_file(kind: &str) -> bool {
    !matches!(kind, "INDEX")
}

fn rel(kind: &str, owner: &str, name: &str) -> String {
    // Links are relative to a file inside objects/<kind>/.
    format!(
        "../{}/{}.{}.md",
        kind.to_lowercase().replace(' ', "_"),
        safe(owner),
        safe(name)
    )
}

fn front_matter(c: &Ctx, n: &GNode, extra: &[(&str, String)]) -> String {
    let mut o = String::from("---\n");
    let _ = writeln!(o, "id: \"{}\"", n.id);
    let _ = writeln!(o, "kind: {}", n.kind.to_lowercase());
    let _ = writeln!(o, "owner: {}", n.owner);
    let _ = writeln!(o, "name: {}", n.name);
    for (k, v) in extra {
        let _ = writeln!(o, "{k}: {v}");
    }
    let (out, inc) = c.edges_of(&n.id);
    let mut rows: Vec<String> = out
        .iter()
        .map(|e| format!("  - {{rel: {}, to: \"{}\"}}", e.kind, e.to))
        .collect();
    rows.extend(
        inc.iter()
            .map(|e| format!("  - {{rel: {}-by, from: \"{}\"}}", e.kind, e.from)),
    );
    if !rows.is_empty() {
        o.push_str("links:\n");
        o.push_str(&rows.join("\n"));
        o.push('\n');
    }
    let _ = writeln!(o, "snapshot: {}", c.kb.taken_at);
    o.push_str("---\n\n");
    o
}

fn key_for(t: &TableFacts, col: &str) -> String {
    let mut k = Vec::new();
    for c in &t.constraints {
        if c.columns.iter().any(|x| x == col) {
            match c.kind.as_str() {
                "P" => k.push("PK".to_string()),
                "U" => k.push("UQ".to_string()),
                "R" => k.push(format!("FK→{}", c.ref_table.as_deref().unwrap_or("?"))),
                _ => {}
            }
        }
    }
    if t.indexes
        .iter()
        .any(|i| i.columns.first().map(String::as_str) == Some(col))
    {
        k.push("IX".into());
    }
    k.join(", ")
}

fn excerpt(body: &str, m: &crate::packages::Member, cap: usize) -> Option<String> {
    if m.line_start == 0 {
        return None;
    }
    let lines: Vec<&str> = body.lines().collect();
    let a = m.line_start.saturating_sub(1).min(lines.len());
    let z = m.line_end.min(lines.len());
    let mut shown: Vec<&str> = lines[a..z].to_vec();
    let mut note = String::new();
    if cap > 0 && shown.len() > cap {
        note = format!(
            "\n-- … {} more lines not shown (member is lines {}–{} of the package body)",
            shown.len() - cap,
            m.line_start,
            m.line_end
        );
        shown.truncate(cap);
    }
    Some(format!("{}{note}", shown.join("\n")))
}

fn package_of<'a>(c: &'a Ctx, n: &GNode) -> Option<&'a PackageDetail> {
    c.kb.packages
        .packages
        .iter()
        .find(|p| p.name == n.name && n.owner == c.kb.owner)
}

fn touching_members<'a>(p: &'a PackageDetail, table: &str) -> Vec<&'a crate::packages::Member> {
    p.members
        .iter()
        .filter(|m| m.table_lines.iter().any(|t| t.table == table))
        .collect()
}

fn table_doc(c: &Ctx, n: &GNode, t: &TableFacts) -> String {
    let mut o = front_matter(c, n, &[("status", t.status.clone())]);
    let _ = writeln!(o, "# {}.{} ({})\n", n.owner, n.name, n.kind.to_lowercase());
    if let Some(cm) = &t.comment {
        let _ = writeln!(o, "> {}\n", cm.replace('\n', " "));
    }
    let mut facts = vec![format!("status **{}**", t.status.to_lowercase())];
    if let Some(r) = t.num_rows {
        facts.push(format!(
            "≈ {r} rows (statistics{})",
            t.last_analyzed
                .as_deref()
                .map(|a| format!(" from {a}"))
                .unwrap_or_default()
        ));
    }
    let _ = writeln!(o, "{}\n", facts.join(" · "));
    o.push_str("## Columns\n\n");
    let rows: Vec<Vec<String>> = t
        .columns
        .iter()
        .enumerate()
        .map(|(i, col)| {
            vec![
                (i + 1).to_string(),
                format!("`{}`", col.name),
                col.data_type.clone(),
                if col.nullable { "yes" } else { "NO" }.into(),
                key_for(t, &col.name),
                col.comment.clone().unwrap_or_default(),
            ]
        })
        .collect();
    o.push_str(&table(
        &["#", "Column", "Type", "Null", "Keys", "Comment"],
        &rows,
    ));
    if !t.constraints.is_empty() {
        o.push_str("\n## Keys and constraints\n\n");
        for k in &t.constraints {
            let label = match k.kind.as_str() {
                "P" => "Primary key",
                "U" => "Unique",
                _ => "Foreign key",
            };
            let target = k.ref_table.as_ref().map(|r| {
                let id = format!("TABLE:{r}");
                format!(" → {} ({})", c.link(&id), k.ref_columns.join(", "))
            });
            let _ = writeln!(
                o,
                "- {label} `{}` ({}){} — {}",
                k.name,
                k.columns.join(", "),
                target.unwrap_or_default(),
                k.status.to_lowercase()
            );
        }
    }
    let (out, inc) = c.edges_of(&n.id);
    let children: Vec<_> = inc.iter().filter(|e| e.kind == "fk").collect();
    if !children.is_empty() {
        o.push_str("\n## Referenced by (foreign keys)\n\n");
        for e in children {
            let _ = writeln!(o, "- {} — {}", c.link(&e.from), c.evidence(&e.from, &e.to));
        }
    }
    if !t.indexes.is_empty() {
        o.push_str("\n## Indexes\n\n");
        let rows: Vec<Vec<String>> = t
            .indexes
            .iter()
            .map(|i| {
                vec![
                    format!("`{}`", i.name),
                    i.columns.join(", "),
                    if i.unique { "unique" } else { "" }.into(),
                    i.index_type.clone(),
                    i.status.to_lowercase(),
                ]
            })
            .collect();
        o.push_str(&table(
            &["Index", "Columns", "Unique", "Type", "Status"],
            &rows,
        ));
    }
    // Triggers on this table.
    let trgs: Vec<_> =
        c.kb.triggers
            .iter()
            .filter(|x| x.table.as_deref() == Some(&n.name))
            .collect();
    if !trgs.is_empty() {
        o.push_str("\n## Triggers\n\n");
        for x in trgs {
            let _ = writeln!(
                o,
                "### `{}` — {} {} ({})\n",
                x.name,
                x.trigger_type.to_lowercase(),
                x.event.to_lowercase(),
                x.status.to_lowercase()
            );
            let id = format!("TRIGGER:{}.{}", n.owner, x.name);
            let seqs: Vec<String> =
                c.kb.edges
                    .iter()
                    .filter(|e| e.from == id && e.to.starts_with("SEQUENCE:"))
                    .map(|e| c.link(&e.to))
                    .collect();
            if !seqs.is_empty() {
                let _ = writeln!(o, "Uses sequence: {}\n", seqs.join(", "));
            }
            if let Some(src) = c.text(false, &n.owner, &x.name, "TRIGGER") {
                o.push_str(&code("plsql", &src));
            }
        }
    }
    // Sequences: directly used by this table (dictionary) or via its triggers.
    let mut seq_ids: BTreeSet<String> = out
        .iter()
        .filter(|e| e.to.starts_with("SEQUENCE:"))
        .map(|e| e.to.clone())
        .collect();
    for x in
        c.kb.triggers
            .iter()
            .filter(|x| x.table.as_deref() == Some(&n.name))
    {
        let id = format!("TRIGGER:{}.{}", n.owner, x.name);
        seq_ids.extend(
            c.kb.edges
                .iter()
                .filter(|e| e.from == id && e.to.starts_with("SEQUENCE:"))
                .map(|e| e.to.clone()),
        );
    }
    if !seq_ids.is_empty() {
        o.push_str("\n## Sequences\n\n");
        for id in &seq_ids {
            let name = id.rsplit('.').next().unwrap_or("");
            let info =
                c.kb.sequences
                    .iter()
                    .find(|s| s.name == name)
                    .map(|s| {
                        format!(
                            " — +{} from {} to {}, cache {}, last {}",
                            s.increment, s.min, s.max, s.cache, s.last
                        )
                    })
                    .unwrap_or_default();
            let _ = writeln!(o, "- {}{info}", c.link(id));
        }
    }
    // Views over it.
    let views: Vec<_> = inc
        .iter()
        .filter(|e| e.from.starts_with("VIEW:") || e.from.starts_with("MATERIALIZED VIEW:"))
        .collect();
    if !views.is_empty() {
        o.push_str("\n## Views over this object\n\n");
        for e in views {
            let _ = writeln!(o, "- {}", c.link(&e.from));
            if let Some(v) = c.node(&e.from) {
                if v.owner == c.kb.owner {
                    if let Some(sql) = c.text(true, &v.owner, &v.name, &v.kind) {
                        o.push_str(&code("sql", &sql));
                    }
                }
            }
        }
    }
    // Packages that touch it, with the code that does.
    let pk_edges: Vec<_> = inc
        .iter()
        .filter(|e| {
            e.from.starts_with("PACKAGE:") && matches!(e.kind.as_str(), "reads" | "writes" | "uses")
        })
        .collect();
    if !pk_edges.is_empty() {
        o.push_str("\n## Packages that use it\n\n");
        for e in pk_edges {
            let pn = c.node(&e.from);
            let _ = writeln!(o, "### {} — {}\n", c.link(&e.from), e.kind);
            let _ = writeln!(o, "Evidence: {}\n", c.evidence(&e.from, &e.to));
            if let Some(p) = pn.and_then(|p| package_of(c, p)) {
                if p.wrapped {
                    o.push_str("_Wrapped package: its code cannot be read; usage comes from dictionary dependencies only._\n\n");
                }
                let body = c.text(false, &c.kb.owner, &p.name, "PACKAGE BODY");
                for m in touching_members(p, &n.name) {
                    let acc: BTreeSet<String> = m
                        .table_lines
                        .iter()
                        .filter(|t| t.table == n.name)
                        .map(|t| {
                            format!(
                                "{} line {}",
                                if t.write { "writes" } else { "reads" },
                                t.line
                            )
                        })
                        .collect();
                    let _ = writeln!(
                        o,
                        "- `{}` — {}",
                        m.signature,
                        acc.into_iter().collect::<Vec<_>>().join(", ")
                    );
                    if let Some(code_text) = body
                        .as_deref()
                        .and_then(|b| excerpt(b, m, c.opts.max_member_lines))
                    {
                        o.push_str(&code("plsql", &code_text));
                    }
                }
            }
        }
    }
    let other: Vec<_> = inc
        .iter()
        .filter(|e| {
            !e.from.starts_with("PACKAGE:")
                && !e.from.starts_with("VIEW:")
                && !e.from.starts_with("MATERIALIZED VIEW:")
                && !matches!(e.kind.as_str(), "fk" | "indexes")
                && !e.from.starts_with("TRIGGER:")
        })
        .collect();
    if !other.is_empty() {
        o.push_str("\n## Other objects that use it\n\n");
        for e in other {
            let _ = writeln!(
                o,
                "- {} ({}) — {}",
                c.link(&e.from),
                e.kind,
                c.evidence(&e.from, &e.to)
            );
        }
    }
    if let Some(gs) = c.kb.grants.get(&n.name) {
        o.push_str("\n## Grants\n\n");
        let mut by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for g in gs {
            by.entry(&g.grantee).or_default().push(&g.privilege);
        }
        for (who, p) in by {
            let _ = writeln!(o, "- {who}: {}", p.join(", "));
        }
    }
    if let Some(ddl) = c.text(true, &n.owner, &n.name, &n.kind) {
        o.push_str("\n## DDL\n\n");
        o.push_str(&code("sql", &ddl));
    }
    o
}

fn package_doc(c: &Ctx, n: &GNode, p: &PackageDetail) -> String {
    let mut o = front_matter(
        c,
        n,
        &[
            (
                "status",
                p.body_status
                    .clone()
                    .unwrap_or_else(|| p.spec_status.clone()),
            ),
            ("wrapped", p.wrapped.to_string()),
        ],
    );
    let _ = writeln!(o, "# {}.{} (package)\n", n.owner, n.name);
    let _ = writeln!(
        o,
        "spec **{}**{} · {} members · {} body lines{}\n",
        p.spec_status.to_lowercase(),
        p.body_status
            .as_ref()
            .map(|b| format!(", body **{}**", b.to_lowercase()))
            .unwrap_or_default(),
        p.members.len(),
        p.body_lines,
        if p.wrapped {
            " · **wrapped (not scanned)**"
        } else {
            ""
        }
    );
    if p.analysis == "plscope" {
        o.push_str("> Calls, reads and writes below come from PL/Scope (exact, with line numbers). Dynamic SQL and callers outside the schema are not seen.\n\n");
    } else {
        o.push_str("> Calls, reads and writes below come from a text scan of the body (heuristic): dynamic SQL and callers outside the schema are not seen. Compile the package with PL/Scope for exact data.\n\n");
    }
    if !p.tables.is_empty() {
        o.push_str("## Tables\n\n");
        for t in &p.tables {
            let id = ["TABLE", "VIEW", "MATERIALIZED VIEW"]
                .iter()
                .map(|k| format!("{k}:{}.{}", n.owner, t.table))
                .find(|id| c.node(id).is_some())
                .unwrap_or_default();
            let _ = writeln!(
                o,
                "- {} — {}",
                if id.is_empty() {
                    t.table.clone()
                } else {
                    c.link(&id)
                },
                if t.read && t.write {
                    "reads and writes"
                } else if t.write {
                    "writes"
                } else {
                    "reads"
                }
            );
        }
    }
    if !p.calls_packages.is_empty() {
        let _ = writeln!(
            o,
            "\n## Calls\n\n{}",
            p.calls_packages
                .iter()
                .map(|x| format!("- {}", c.link(&format!("PACKAGE:{}.{}", n.owner, x))))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    if !p.called_by.is_empty() {
        let _ = writeln!(
            o,
            "\n## Called by\n\n{}",
            p.called_by
                .iter()
                .map(|x| format!("- {}", c.link(&format!("PACKAGE:{}.{}", n.owner, x))))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let (_, inc) = c.edges_of(&n.id);
    let ext: Vec<_> = inc
        .iter()
        .filter(|e| !e.from.starts_with("PACKAGE:") || !e.from.contains(&format!(":{}.", n.owner)))
        .collect();
    if !ext.is_empty() {
        o.push_str("\n## Also used by\n\n");
        for e in ext {
            let _ = writeln!(o, "- {} ({})", c.link(&e.from), e.kind);
        }
    }
    let is_focus = c.opts.focus == n.id;
    let touches_view = |m: &crate::packages::Member| {
        m.table_lines.iter().any(|t| {
            ["TABLE", "VIEW", "MATERIALIZED VIEW"]
                .iter()
                .any(|k| c.in_view(&format!("{k}:{}.{}", n.owner, t.table)))
        })
    };
    let small = p.body_lines <= 400;
    if let Some(spec) = c.text(false, &n.owner, &n.name, "PACKAGE") {
        o.push_str("\n## Specification\n\n");
        o.push_str(&code("plsql", &spec));
    }
    let body = c.text(false, &n.owner, &n.name, "PACKAGE BODY");
    o.push_str("\n## Members\n\n");
    if body.is_some() && !is_focus && !small {
        o.push_str("_Code is shown for members that touch a table in this export; open the package on its own for all of it._\n\n");
    }
    for m in &p.members {
        let _ = writeln!(
            o,
            "### `{}` — {} {}\n",
            m.name,
            if m.public { "public" } else { "private" },
            m.kind.to_lowercase()
        );
        let _ = writeln!(o, "`{}`\n", m.signature);
        let mut facts: Vec<String> = Vec::new();
        if !m.reads.is_empty() {
            facts.push(format!("reads {}", m.reads.join(", ")));
        }
        if !m.writes.is_empty() {
            facts.push(format!("writes {}", m.writes.join(", ")));
        }
        if !m.calls.is_empty() {
            facts.push(format!(
                "calls {}",
                m.calls
                    .iter()
                    .map(|x| format!("{}.{}", x.package, x.member))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !m.called_by.is_empty() {
            facts.push(format!(
                "called by {}",
                m.called_by
                    .iter()
                    .map(|x| format!("{}.{}", x.package, x.member))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if m.internal_refs > 0 {
            facts.push(format!("{} internal call(s)", m.internal_refs));
        }
        if !m.dynamic_sql_lines.is_empty() {
            facts.push(format!(
                "**dynamic SQL at line(s) {}** (what it touches is unknown)",
                m.dynamic_sql_lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !m.scan_only.is_empty() {
            facts.push(format!(
                "the text scan also saw (PL/Scope did not): {}",
                m.scan_only.join(", ")
            ));
        }
        facts.push(match m.status.as_str() {
            "unreferenced" => "no caller found in this schema".into(),
            "unused_private" => "private and never called".into(),
            _ => "used".into(),
        });
        let _ = writeln!(o, "{}\n", facts.join(" · "));
        if let Some(b) = &body {
            if is_focus || small || touches_view(m) {
                if let Some(t) = excerpt(b, m, c.opts.max_member_lines) {
                    o.push_str(&code("plsql", &t));
                }
            }
        }
    }
    o
}

fn simple_doc(c: &Ctx, n: &GNode) -> String {
    let mut o = front_matter(c, n, &[]);
    let _ = writeln!(o, "# {}.{} ({})\n", n.owner, n.name, n.kind.to_lowercase());
    let (out, inc) = c.edges_of(&n.id);
    match n.kind.as_str() {
        "SEQUENCE" => {
            if let Some(s) =
                c.kb.sequences
                    .iter()
                    .find(|s| s.name == n.name && n.owner == c.kb.owner)
            {
                let _ = writeln!(
                    o,
                    "Increment {}, range {} to {}{}, cache {}, last number {}.\n",
                    s.increment,
                    s.min,
                    s.max,
                    if s.cycle { ", cycles" } else { "" },
                    s.cache,
                    s.last
                );
            }
        }
        "TRIGGER" => {
            if let Some(t) =
                c.kb.triggers
                    .iter()
                    .find(|t| t.name == n.name && n.owner == c.kb.owner)
            {
                let _ = writeln!(
                    o,
                    "{} {} on {} — {}\n",
                    t.trigger_type.to_lowercase(),
                    t.event.to_lowercase(),
                    t.table
                        .as_deref()
                        .map(|x| c.link(&format!("TABLE:{}.{}", n.owner, x)))
                        .unwrap_or_else(|| "schema".into()),
                    t.status.to_lowercase()
                );
            }
        }
        "SYNONYM" => {
            if let Some(s) =
                c.kb.synonyms
                    .iter()
                    .find(|s| s.name == n.name && n.owner == c.kb.owner)
            {
                let _ = writeln!(
                    o,
                    "Synonym for {}.{}{}\n",
                    s.target_owner,
                    s.target,
                    s.db_link
                        .as_deref()
                        .map(|l| format!(" over database link `{l}` (not followed)"))
                        .unwrap_or_default()
                );
            }
        }
        "JOB" => {
            if let Some(j) = c.kb.jobs.iter().find(|j| j.name == n.name) {
                let _ = writeln!(
                    o,
                    "{} job, {}. Action: `{}`\n",
                    j.job_type.to_lowercase(),
                    if j.enabled { "enabled" } else { "disabled" },
                    j.action.trim()
                );
            }
        }
        _ => {}
    }
    for (title, edges, take_from) in [("Uses", &out, false), ("Used by", &inc, true)] {
        if edges.is_empty() {
            continue;
        }
        let _ = writeln!(o, "## {title}\n");
        for e in edges.iter() {
            let other = if take_from { &e.from } else { &e.to };
            let _ = writeln!(
                o,
                "- {} ({}) — {}",
                c.link(other),
                e.kind,
                c.evidence(&e.from, &e.to)
            );
        }
        o.push('\n');
    }
    let ddl_kind = match n.kind.as_str() {
        "TRIGGER" | "PROCEDURE" | "FUNCTION" => Some("src"),
        "TYPE" => Some("ddl"),
        _ => None,
    };
    if n.owner == c.kb.owner {
        match ddl_kind {
            Some("src") => {
                if let Some(t) = c.text(false, &n.owner, &n.name, &n.kind) {
                    o.push_str("## Source\n\n");
                    o.push_str(&code("plsql", &t));
                }
            }
            Some(_) => {
                if let Some(t) = c.text(true, &n.owner, &n.name, &n.kind) {
                    o.push_str("## DDL\n\n");
                    o.push_str(&code("sql", &t));
                }
            }
            None => {}
        }
    }
    o
}

fn doc(c: &Ctx, n: &GNode) -> String {
    match n.kind.as_str() {
        "TABLE" | "VIEW" | "MATERIALIZED VIEW" if n.owner == c.kb.owner => {
            match c.kb.table(&n.name) {
                Some(t) => table_doc(c, n, t),
                None => simple_doc(c, n),
            }
        }
        "PACKAGE" if n.owner == c.kb.owner => match package_of(c, n) {
            Some(p) => package_doc(c, n, p),
            None => simple_doc(c, n),
        },
        _ => simple_doc(c, n),
    }
}

/// Builds the files for `opts.focus` and its neighbourhood.
pub fn build(kb: &Knowledge, fetch: &dyn Fetch, opts: &DossierOptions) -> Result<Bundle> {
    let graph = kb.graph();
    let sub = graph
        .focus(&Focus {
            id: opts.focus.clone(),
            direction: opts.direction,
            hops: opts.hops,
            kinds: None,
            max_nodes: opts.max_nodes,
        })
        .ok_or_else(|| Error::Invalid(format!("{} is not in the knowledge base", opts.focus)))?;
    // Sequences used by triggers in the export belong with them even when they are not on the
    // chosen side of the graph.
    let mut ids: BTreeSet<&str> = sub.nodes.iter().map(|n| n.node.id.as_str()).collect();
    let mut context: Vec<&GNode> = Vec::new();
    for e in &kb.edges {
        if e.from.starts_with("TRIGGER:")
            && e.to.starts_with("SEQUENCE:")
            && ids.contains(e.from.as_str())
            && !ids.contains(e.to.as_str())
        {
            if let Some(n) = kb.nodes.iter().find(|n| n.id == e.to) {
                ids.insert(n.id.as_str());
                context.push(n);
            }
        }
    }
    let ctx = Ctx {
        kb,
        fetch,
        opts,
        ids,
        sub: &sub,
        cache: RefCell::default(),
        missing: RefCell::default(),
    };
    let mut files: Vec<BundleFile> = Vec::new();
    let mut listed: Vec<(&GNode, String)> = Vec::new();
    let mut ordered: Vec<&crate::graph::SNode> = sub
        .nodes
        .iter()
        .filter(|n| has_file(&n.node.kind))
        .collect();
    // Focus first, then by distance, so the most relevant content leads the combined file.
    ordered.sort_by_key(|n| {
        (
            n.node.id != opts.focus,
            n.depth.abs(),
            n.node.kind.clone(),
            n.node.name.clone(),
        )
    });
    for sn in ordered {
        let path = file_for(&sn.node);
        files.push(BundleFile {
            path: path.clone(),
            content: doc(&ctx, &sn.node),
        });
        listed.push((&sn.node, path));
    }
    for n in context {
        let path = file_for(n);
        files.push(BundleFile {
            path: path.clone(),
            content: doc(&ctx, n),
        });
        listed.push((n, path));
    }
    // Index.
    let focus_node = ctx.node(&opts.focus);
    let mut idx = String::from("---\ntype: ora-ide-knowledge-export\n");
    let _ = writeln!(
        idx,
        "focus: \"{}\"\nowner: {}\nsnapshot: {}\ndirection: {:?}\nhops: {}\n---\n",
        opts.focus, kb.owner, kb.taken_at, opts.direction, opts.hops
    );
    let _ = writeln!(
        idx,
        "# Knowledge export: {}\n",
        focus_node
            .map(|n| format!("{}.{} ({})", n.owner, n.name, n.kind.to_lowercase()))
            .unwrap_or_default()
    );
    let _ = writeln!(idx, "{} objects within {} hop(s), direction **{}**{}. Row data is not included; secrets in source are masked.\n", sub.nodes.len(), opts.hops, match opts.direction { Direction::Uses => "what it uses", Direction::UsedBy => "what uses it", Direction::Both => "both ways" }, if sub.truncated { " (**truncated**: more objects exist)" } else { "" });
    idx.push_str("## How to read this\n\n- Every link has a source and a certainty: **dictionary / exact** (Oracle's data dictionary), **plscope / exact** (PL/Scope, with line numbers) or **scan / heuristic** (found by reading package code; can miss dynamic SQL).\n- `links:` in each file's header lists the graph edges (`rel`) so tools can follow them; `graph.json` has them all.\n\n");
    idx.push_str("## Objects\n\n");
    let rows: Vec<Vec<String>> = listed
        .iter()
        .map(|(n, p)| {
            vec![
                n.kind.to_lowercase(),
                format!("[{}.{}]({})", n.owner, n.name, p),
                sub.nodes
                    .iter()
                    .find(|s| s.node.id == n.id)
                    .map(|s| s.depth.to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    idx.push_str(&table(&["Kind", "Object", "Depth"], &rows));
    idx.push_str("\n## What this export cannot tell you\n\n");
    for g in &kb.coverage.gaps {
        let _ = writeln!(idx, "- **{}** ({}): {}", g.area, g.severity, g.message);
    }
    let missing: Vec<String> = ctx.missing.borrow().iter().cloned().collect();
    if !missing.is_empty() {
        idx.push_str("\n## Text that could not be read\n\n");
        for m in &missing {
            let _ = writeln!(idx, "- {m}");
        }
    }
    files.insert(
        0,
        BundleFile {
            path: "README.md".into(),
            content: idx,
        },
    );

    // Machine-readable graph with evidence.
    let ev: Vec<&crate::knowledge::Evidence> = kb
        .evidence
        .iter()
        .filter(|e| ctx.in_view(&e.from) && ctx.in_view(&e.to))
        .collect();
    let graph_json = serde_json::json!({
        "focus": opts.focus,
        "nodes": sub.nodes,
        "edges": sub.edges,
        "evidence": ev,
    });
    files.push(BundleFile {
        path: "graph.json".into(),
        content: serde_json::to_string_pretty(&graph_json).unwrap_or_default(),
    });
    let chars: usize = files
        .iter()
        .filter(|f| f.path.ends_with(".md"))
        .map(|f| f.content.chars().count())
        .sum();
    let manifest = serde_json::json!({
        "format": 1,
        "owner": kb.owner,
        "snapshot": kb.taken_at,
        "focus": opts.focus,
        "objects": listed.len(),
        "files": files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        "tokens_estimate": chars / 4,
        "truncated": sub.truncated,
        "missing": missing,
    });
    files.push(BundleFile {
        path: "manifest.json".into(),
        content: serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    });
    let _ = ctx.sub;
    Ok(Bundle {
        files,
        tokens_estimate: chars / 4,
        truncated: sub.truncated,
        missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::collect_schema;
    use crate::knowledge::tests::db;
    use std::cell::Cell;
    use std::collections::HashMap;

    struct Texts {
        map: HashMap<String, String>,
        calls: Cell<usize>,
    }
    impl Texts {
        fn new() -> Self {
            let mut map = HashMap::new();
            map.insert(
                "TABLE:EMPLOYEES".into(),
                "CREATE TABLE \"HR\".\"EMPLOYEES\" (\"EMPLOYEE_ID\" NUMBER(6))".into(),
            );
            map.insert(
                "VIEW:EMP_V".into(),
                "CREATE VIEW emp_v AS SELECT employee_id FROM employees".into(),
            );
            map.insert("TRIGGER:EMP_BI".into(), "TRIGGER emp_bi BEFORE INSERT ON employees\n-- ALTER USER app IDENTIFIED BY hunter2\nBEGIN :new.employee_id := emp_seq.NEXTVAL; END;".into());
            map.insert(
                "PACKAGE:EMP_PKG".into(),
                "PACKAGE emp_pkg IS PROCEDURE raise_all; END;".into(),
            );
            map.insert("PACKAGE BODY:EMP_PKG".into(), "PACKAGE BODY emp_pkg IS\n  PROCEDURE raise_all IS BEGIN\n    UPDATE employees SET x = 1;\n    EXECUTE IMMEDIATE 'ANALYZE';\n  END;\nEND;\n".into());
            Self {
                map,
                calls: Cell::new(0),
            }
        }
        fn get(&self, name: &str, kind: &str) -> Result<String> {
            self.calls.set(self.calls.get() + 1);
            self.map
                .get(&format!("{kind}:{name}"))
                .cloned()
                .ok_or_else(|| Error::Database(format!("ORA-31603: object {name} not found")))
        }
    }
    impl Fetch for Texts {
        fn source(&self, _o: &str, name: &str, kind: &str) -> Result<String> {
            self.get(name, kind)
        }
        fn ddl(&self, _o: &str, name: &str, kind: &str) -> Result<String> {
            self.get(name, kind)
        }
    }

    fn opts(focus: &str) -> DossierOptions {
        DossierOptions {
            focus: focus.into(),
            direction: Direction::Both,
            hops: 2,
            include_source: true,
            max_member_lines: 0,
            max_nodes: None,
        }
    }
    fn kb() -> Knowledge {
        collect_schema(&db(true), "HR", 1_700_000_000_000).unwrap()
    }
    fn file<'a>(b: &'a Bundle, path: &str) -> &'a str {
        &b.files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| {
                panic!(
                    "{path} missing from {:?}",
                    b.files.iter().map(|f| &f.path).collect::<Vec<_>>()
                )
            })
            .content
    }

    #[test]
    fn a_table_dossier_has_everything_that_touches_it() {
        let t = Texts::new();
        let b = build(&kb(), &t, &opts("TABLE:HR.EMPLOYEES")).unwrap();
        let e = file(&b, "objects/table/HR.EMPLOYEES.md");
        assert!(e.contains("id: \"TABLE:HR.EMPLOYEES\""));
        assert!(
            e.contains("| 1 | `EMPLOYEE_ID` | NUMBER(6) | NO | PK | PK |"),
            "{e}"
        );
        assert!(
            e.contains("FK→HR.DEPARTMENTS"),
            "foreign key shown in the column keys"
        );
        assert!(e.contains("Foreign key `EMP_DEPT_FK` (DEPT_ID) → [HR.DEPARTMENTS](../table/HR.DEPARTMENTS.md) (DEPT_ID)"), "{e}");
        assert!(e.contains("`EMP_DEPT_IX`") && e.contains("## Indexes"));
        assert!(e.contains("### `EMP_BI` — before each row insert (enabled)"));
        assert!(
            e.contains("Uses sequence: [HR.EMP_SEQ](../sequence/HR.EMP_SEQ.md)"),
            "{e}"
        );
        assert!(e.contains("## Sequences") && e.contains("last 207"));
        assert!(e.contains("## Views over this object") && e.contains("CREATE VIEW emp_v"));
        assert!(
            e.contains("[HR.EMP_PKG](../package/HR.EMP_PKG.md) — writes"),
            "{e}"
        );
        assert!(
            e.contains("writes line 3") && e.contains("UPDATE employees SET x = 1;"),
            "member code excerpt: {e}"
        );
        assert!(e.contains("[dictionary, exact]") && e.contains("[scan, heuristic]"));
        assert!(e.contains("- APP_RO: SELECT"));
        assert!(e.contains("## DDL") && e.contains("CREATE TABLE"));
        assert!(!e.contains("hunter2"), "secrets must be masked");
        assert!(
            e.contains("Other objects that use it") && e.contains("[HR.EMP](../synonym/HR.EMP.md)")
        );
        assert!(
            e.contains("[REPORTS.MONTHLY_V](../view/REPORTS.MONTHLY_V.md)"),
            "views in other schemas are listed"
        );
        let r = file(&b, "README.md");
        assert!(
            r.contains("What this export cannot tell you")
                && r.contains("wrapped")
                && r.contains("dynamic SQL")
        );
        assert!(
            b.files.iter().any(|f| f.path == "graph.json")
                && b.files.iter().any(|f| f.path == "manifest.json")
        );
        let g: serde_json::Value = serde_json::from_str(file(&b, "graph.json")).unwrap();
        assert!(g["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["source"] == "scan"));
    }

    #[test]
    fn package_dossier_shows_members_calls_and_dynamic_sql() {
        let b = build(&kb(), &Texts::new(), &opts("PACKAGE:HR.EMP_PKG")).unwrap();
        let p = file(&b, "objects/package/HR.EMP_PKG.md");
        assert!(p.contains("### `RAISE_ALL` — public procedure"));
        assert!(
            p.contains("writes EMPLOYEES") && p.contains("dynamic SQL at line(s) 4"),
            "{p}"
        );
        assert!(p.contains("## Specification") && p.contains("PACKAGE emp_pkg IS"));
        assert!(p.contains("UPDATE employees SET x = 1;"));
        assert!(p.contains("no caller found in this schema"));
        let w = build(&kb(), &Texts::new(), &opts("PACKAGE:HR.SECRET_PKG")).unwrap();
        assert!(file(&w, "objects/package/HR.SECRET_PKG.md").contains("wrapped (not scanned)"));
    }

    #[test]
    fn without_source_nothing_is_fetched_and_no_code_is_written() {
        let t = Texts::new();
        let mut o = opts("TABLE:HR.EMPLOYEES");
        o.include_source = false;
        let b = build(&kb(), &t, &o).unwrap();
        assert_eq!(t.calls.get(), 0);
        assert!(b
            .files
            .iter()
            .filter(|f| f.path.ends_with(".md"))
            .all(|f| !f.content.contains("```")));
    }

    #[test]
    fn unreadable_text_is_reported_not_hidden() {
        let mut t = Texts::new();
        t.map.remove("TRIGGER:EMP_BI");
        let b = build(&kb(), &t, &opts("TABLE:HR.EMPLOYEES")).unwrap();
        assert!(
            b.missing
                .iter()
                .any(|m| m.contains("TRIGGER HR.EMP_BI") && m.contains("ORA-31603")),
            "{:?}",
            b.missing
        );
        assert!(file(&b, "README.md").contains("Text that could not be read"));
    }

    #[test]
    fn member_code_is_capped_and_says_so() {
        let mut o = opts("PACKAGE:HR.EMP_PKG");
        o.max_member_lines = 2;
        let b = build(&kb(), &Texts::new(), &o).unwrap();
        assert!(file(&b, "objects/package/HR.EMP_PKG.md").contains("more lines not shown"));
    }

    #[test]
    fn combined_markdown_and_zip_are_written_atomically() {
        let b = build(&kb(), &Texts::new(), &opts("TABLE:HR.EMPLOYEES")).unwrap();
        let c = b.combined();
        assert!(
            c.starts_with("<!-- file: README.md -->")
                && c.contains("<!-- file: objects/table/HR.EMPLOYEES.md -->")
                && !c.contains("graph.json -->")
        );
        let dir = std::env::temp_dir().join(format!("ora-ide-dossier-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        b.write_zip(&dir.join("k.zip")).unwrap();
        b.write_combined(&dir.join("k.md")).unwrap();
        assert!(!dir.join("k.part").exists());
        let mut z = zip::ZipArchive::new(std::fs::File::open(dir.join("k.zip")).unwrap()).unwrap();
        let names: Vec<String> = (0..z.len())
            .map(|i| z.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        assert!(
            names.contains(&"README.md".to_string())
                && names.contains(&"manifest.json".to_string())
                && names.contains(&"objects/table/HR.EMPLOYEES.md".to_string())
        );
        assert_eq!(std::fs::read_to_string(dir.join("k.md")).unwrap(), c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_focus_is_an_error_and_depth_one_is_smaller() {
        assert!(build(&kb(), &Texts::new(), &opts("TABLE:HR.NOPE")).is_err());
        let wide = build(&kb(), &Texts::new(), &opts("TABLE:HR.EMPLOYEES")).unwrap();
        let mut o = opts("TABLE:HR.EMPLOYEES");
        o.hops = 1;
        o.direction = Direction::Uses;
        let narrow = build(&kb(), &Texts::new(), &o).unwrap();
        assert!(narrow.files.len() < wide.files.len());
        assert!(wide.tokens_estimate > 0);
    }
}
