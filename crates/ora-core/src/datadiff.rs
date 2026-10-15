//! Key-based comparison of two tables of rows: files (CSV/Excel), queries or tables.
//!
//! A line diff is the wrong tool for data (one inserted row shifts everything), so rows are matched
//! by key and compared column by column. Values are compared by *meaning*, not spelling:
//! `1`, `1.0` and `+1` are the same number; `2021-05-01` and `2021-05-01 00:00:00` the same date;
//! an empty cell equals NULL (a CSV cannot tell them apart; Oracle cannot either for `''`).
//!
//! Memory is bounded: when side A does not fit in `memory_rows`, both sides are spilled to
//! hash partitions on disk and compared one partition at a time.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Cursor;
use crate::import::profile::{canonical_num, family, parse_date, parse_num};
use crate::import::source::{open_source, ReadOptions, RowSource};
use crate::{Error, Result};

pub type Cell = Option<String>;

/// A stream of rows with named columns.
pub trait RowStream {
    fn columns(&self) -> &[String];
    fn next_row(&mut self) -> Result<Option<Vec<Cell>>>;
}

pub struct FileStream {
    src: Box<dyn RowSource>,
    cols: Vec<String>,
}

impl FileStream {
    pub fn open(path: &Path, opts: &ReadOptions) -> Result<Self> {
        let src = open_source(path, opts)?;
        let cols = src.headers().to_vec();
        Ok(Self { src, cols })
    }
}

impl RowStream for FileStream {
    fn columns(&self) -> &[String] {
        &self.cols
    }
    fn next_row(&mut self) -> Result<Option<Vec<Cell>>> {
        Ok(self
            .src
            .next_row()?
            .map(|r| r.into_iter().map(Some).collect()))
    }
}

pub struct CursorStream {
    cursor: Box<dyn Cursor>,
    buf: VecDeque<Vec<Cell>>,
    cols: Vec<String>,
}

impl CursorStream {
    pub fn new(cursor: Box<dyn Cursor>) -> Self {
        let cols = cursor.columns().iter().map(|c| c.name.clone()).collect();
        Self {
            cursor,
            buf: VecDeque::new(),
            cols,
        }
    }
}

impl RowStream for CursorStream {
    fn columns(&self) -> &[String] {
        &self.cols
    }
    fn next_row(&mut self) -> Result<Option<Vec<Cell>>> {
        if self.buf.is_empty() {
            self.buf.extend(self.cursor.next_batch(5000)?);
        }
        Ok(self.buf.pop_front())
    }
}

/// In-memory rows, for tests and small inputs.
pub struct VecStream {
    cols: Vec<String>,
    rows: std::vec::IntoIter<Vec<Cell>>,
}

impl VecStream {
    pub fn new(cols: &[&str], rows: Vec<Vec<Cell>>) -> Self {
        Self {
            cols: cols.iter().map(|c| c.to_string()).collect(),
            rows: rows.into_iter(),
        }
    }
}

impl RowStream for VecStream {
    fn columns(&self) -> &[String] {
        &self.cols
    }
    fn next_row(&mut self) -> Result<Option<Vec<Cell>>> {
        Ok(self.rows.next())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataDiffOptions {
    /// Key columns (by name, case-insensitive). Empty: compare whole rows (no "changed", only added/removed).
    pub keys: Vec<String>,
    pub trim: bool,
    pub ignore_case: bool,
    pub empty_is_null: bool,
    /// Compare numbers by value.
    pub numeric: bool,
    /// Compare ISO dates/timestamps (`YYYY-MM-DD[ T]HH:MI:SS[.f]`) by value.
    pub dates: bool,
    /// Decimal mark of numbers in each side (`.` or `,`).
    pub a_decimal: char,
    pub b_decimal: char,
    pub max_samples: usize,
    /// Rows of side A held in memory before spilling to disk.
    pub memory_rows: usize,
    pub partitions: usize,
}

impl Default for DataDiffOptions {
    fn default() -> Self {
        Self {
            keys: vec![],
            trim: true,
            ignore_case: false,
            empty_is_null: true,
            numeric: true,
            dates: true,
            a_decimal: '.',
            b_decimal: '.',
            max_samples: 200,
            memory_rows: 1_000_000,
            partitions: 256,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CellDiff {
    pub column: String,
    pub a: Cell,
    pub b: Cell,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangedRow {
    pub key: Vec<Cell>,
    pub diffs: Vec<CellDiff>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DataDiffReport {
    pub key_columns: Vec<String>,
    pub whole_row: bool,
    pub compared_columns: Vec<String>,
    pub only_in_a_columns: Vec<String>,
    pub only_in_b_columns: Vec<String>,
    pub rows_a: u64,
    pub rows_b: u64,
    pub same: u64,
    pub changed: u64,
    pub only_a: u64,
    pub only_b: u64,
    pub duplicate_keys_a: u64,
    pub duplicate_keys_b: u64,
    pub empty_keys_a: u64,
    pub empty_keys_b: u64,
    /// `(column, rows that differ in it)`, most frequent first.
    pub by_column: Vec<(String, u64)>,
    /// Column order of the sample rows: keys first.
    pub layout: Vec<String>,
    pub changed_samples: Vec<ChangedRow>,
    pub only_a_samples: Vec<Vec<Cell>>,
    pub only_b_samples: Vec<Vec<Cell>>,
    pub diff_file: Option<String>,
    pub spilled: bool,
    pub cancelled: bool,
    pub identical: bool,
    pub elapsed_ms: u64,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------- value meaning

struct Canon<'a> {
    o: &'a DataDiffOptions,
}

impl Canon<'_> {
    fn value(&self, v: &Cell, decimal: char) -> Cell {
        let mut s = v.as_deref()?;
        if self.o.trim {
            s = s.trim();
        }
        if s.is_empty() && self.o.empty_is_null {
            return None;
        }
        if self.o.numeric {
            if let Ok(mut p) = parse_num(s, decimal) {
                // 1.50 and 1.5 are the same number.
                p.frac = p.frac.trim_end_matches('0').to_string();
                return Some(format!("n:{}", canonical_num(&p)));
            }
        }
        if self.o.dates {
            if let Some(d) = iso_date(s) {
                return Some(format!("d:{d}"));
            }
        }
        Some(if self.o.ignore_case {
            format!("t:{}", s.to_lowercase())
        } else {
            format!("t:{s}")
        })
    }
}

/// `YYYY-MM-DD`, optionally with `HH:MM[:SS[.f]]` after a space or `T`; `None` for anything else.
fn iso_date(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 10 || !(b[4] == b'-' && b[7] == b'-') || !b[..4].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let text = if b.len() > 10 && b[10] == b'T' {
        format!("{} {}", &s[..10], &s[11..])
    } else {
        s.to_string()
    };
    let (dt, _) = parse_date(&text, family("YYYY-MM-DD")?)?;
    let mut out = dt.format("%Y-%m-%d %H:%M:%S").to_string();
    let nanos = dt.and_utc().timestamp_subsec_nanos();
    if nanos != 0 {
        out.push_str(format!(".{nanos:09}").trim_end_matches('0'));
    }
    Some(out)
}

/// Spelling-independent identity of a value, for keys.
fn key_of(parts: &[Cell]) -> Option<String> {
    let mut k = String::new();
    for p in parts {
        k.push_str(p.as_deref()?);
        k.push('\u{1f}');
    }
    Some(k)
}

// ------------------------------------------------------------------------------- spilling

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p =
            std::env::temp_dir().join(format!("ora-ide-compare-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).map_err(io)?;
        Ok(Self(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn io(e: std::io::Error) -> Error {
    Error::Invalid(format!("comparison temp file: {e}"))
}

fn put_row(w: &mut impl Write, row: &[Cell]) -> std::io::Result<()> {
    w.write_all(&(row.len() as u32).to_le_bytes())?;
    for c in row {
        match c {
            None => w.write_all(&[0])?,
            Some(s) => {
                w.write_all(&[1])?;
                w.write_all(&(s.len() as u32).to_le_bytes())?;
                w.write_all(s.as_bytes())?;
            }
        }
    }
    Ok(())
}

fn get_row(r: &mut impl Read) -> std::io::Result<Option<Vec<Cell>>> {
    let mut n = [0u8; 4];
    match r.read_exact(&mut n) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(n) as usize;
    let mut row = Vec::with_capacity(n);
    for _ in 0..n {
        let mut f = [0u8; 1];
        r.read_exact(&mut f)?;
        if f[0] == 0 {
            row.push(None);
        } else {
            let mut l = [0u8; 4];
            r.read_exact(&mut l)?;
            let mut buf = vec![0u8; u32::from_le_bytes(l) as usize];
            r.read_exact(&mut buf)?;
            row.push(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
    }
    Ok(Some(row))
}

fn partition_of(key: &str, parts: usize) -> usize {
    let h = Sha256::digest(key.as_bytes());
    (u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize) % parts
}

struct Spill {
    dir: TempDir,
    a: Vec<BufWriter<File>>,
    b: Vec<BufWriter<File>>,
}

impl Spill {
    fn new(parts: usize) -> Result<Self> {
        let dir = TempDir::new()?;
        let open = |side: &str, i: usize| -> Result<BufWriter<File>> {
            Ok(BufWriter::new(
                File::create(dir.0.join(format!("{side}{i}"))).map_err(io)?,
            ))
        };
        let a = (0..parts)
            .map(|i| open("a", i))
            .collect::<Result<Vec<_>>>()?;
        let b = (0..parts)
            .map(|i| open("b", i))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { dir, a, b })
    }
}

// --------------------------------------------------------------------------------- compare

struct Entry {
    vals: Vec<Cell>,
    matched: bool,
}

struct Plan {
    /// For the layout (keys first, then compared columns): index into A's and B's rows.
    ia: Vec<usize>,
    ib: Vec<usize>,
    nkeys: usize,
    layout: Vec<String>,
}

struct Run<'a> {
    o: &'a DataDiffOptions,
    plan: &'a Plan,
    canon: Canon<'a>,
    rep: DataDiffReport,
    by_col: HashMap<String, u64>,
    out: Option<csv::Writer<File>>,
    out_err: Option<String>,
}

impl Run<'_> {
    fn project(&self, row: &[Cell], side_a: bool) -> Vec<Cell> {
        let idx = if side_a { &self.plan.ia } else { &self.plan.ib };
        idx.iter()
            .map(|&i| row.get(i).cloned().unwrap_or(None))
            .collect()
    }

    fn canon_row(&self, vals: &[Cell], side_a: bool) -> Vec<Cell> {
        let dec = if side_a {
            self.o.a_decimal
        } else {
            self.o.b_decimal
        };
        vals.iter().map(|v| self.canon.value(v, dec)).collect()
    }

    fn write(&mut self, change: &str, vals: &[Cell], column: &str, a: &Cell, b: &Cell) {
        let Some(w) = self.out.as_mut() else { return };
        let mut rec: Vec<String> = vec![change.to_string()];
        rec.extend(
            vals[..self.plan.nkeys]
                .iter()
                .map(|v| v.clone().unwrap_or_default()),
        );
        rec.push(column.to_string());
        rec.push(a.clone().unwrap_or_default());
        rec.push(b.clone().unwrap_or_default());
        if let Err(e) = w.write_record(&rec) {
            self.out_err.get_or_insert(e.to_string());
            self.out = None;
        }
    }

    fn unmatched(&mut self, vals: &[Cell], side_a: bool) {
        let n = self.plan.nkeys;
        if side_a {
            self.rep.only_a += 1;
            if self.rep.only_a_samples.len() < self.o.max_samples {
                self.rep.only_a_samples.push(vals.to_vec());
            }
        } else {
            self.rep.only_b += 1;
            if self.rep.only_b_samples.len() < self.o.max_samples {
                self.rep.only_b_samples.push(vals.to_vec());
            }
        }
        let change = if side_a { "only_a" } else { "only_b" };
        if vals.len() == n {
            self.write(change, vals, "", &None, &None);
        }
        for (i, name) in self.plan.layout.clone().iter().enumerate().skip(n) {
            let (a, b) = if side_a {
                (vals[i].clone(), None)
            } else {
                (None, vals[i].clone())
            };
            self.write(change, vals, name, &a, &b);
        }
    }

    /// Match one B row against the entry for its key.
    fn matched(&mut self, e: &mut Entry, brow: Vec<Cell>) {
        if e.matched {
            self.rep.duplicate_keys_b += 1;
            return;
        }
        e.matched = true;
        let n = self.plan.nkeys;
        let (ca, cb) = (self.canon_row(&e.vals, true), self.canon_row(&brow, false));
        let mut diffs = Vec::new();
        for i in n..ca.len() {
            if ca[i] != cb[i] {
                diffs.push(CellDiff {
                    column: self.plan.layout[i].clone(),
                    a: e.vals[i].clone(),
                    b: brow[i].clone(),
                });
            }
        }
        if diffs.is_empty() {
            self.rep.same += 1;
            return;
        }
        self.rep.changed += 1;
        for d in &diffs {
            *self.by_col.entry(d.column.clone()).or_default() += 1;
            let (name, a, b) = (d.column.clone(), d.a.clone(), d.b.clone());
            self.write("changed", &e.vals, &name, &a, &b);
        }
        if self.rep.changed_samples.len() < self.o.max_samples {
            self.rep.changed_samples.push(ChangedRow {
                key: e.vals[..n].to_vec(),
                diffs,
            });
        }
    }
}

fn norm_name(s: &str) -> String {
    s.trim().to_lowercase()
}

fn plan(a: &[String], b: &[String], keys: &[String], rep: &mut DataDiffReport) -> Result<Plan> {
    let index = |cols: &[String], side: &str| -> Result<HashMap<String, usize>> {
        let mut m = HashMap::new();
        for (i, c) in cols.iter().enumerate() {
            if m.insert(norm_name(c), i).is_some() {
                return Err(Error::Invalid(format!(
                    "column {c:?} appears twice in {side}"
                )));
            }
        }
        Ok(m)
    };
    let (ma, mb) = (index(a, "side A")?, index(b, "side B")?);
    let common: Vec<(String, usize, usize)> = a
        .iter()
        .enumerate()
        .filter_map(|(i, c)| mb.get(&norm_name(c)).map(|&j| (c.clone(), i, j)))
        .collect();
    rep.only_in_a_columns = a
        .iter()
        .filter(|c| !mb.contains_key(&norm_name(c)))
        .cloned()
        .collect();
    rep.only_in_b_columns = b
        .iter()
        .filter(|c| !ma.contains_key(&norm_name(c)))
        .cloned()
        .collect();
    if common.is_empty() {
        return Err(Error::Invalid(
            "the two sides have no column name in common".into(),
        ));
    }
    let mut key_idx: Vec<usize> = Vec::new();
    for k in keys {
        match common
            .iter()
            .position(|(n, _, _)| norm_name(n) == norm_name(k))
        {
            Some(p) if !key_idx.contains(&p) => key_idx.push(p),
            Some(_) => {}
            None => {
                return Err(Error::Invalid(format!(
                    "key column {k:?} is not in both sides"
                )))
            }
        }
    }
    rep.whole_row = key_idx.is_empty();
    if rep.whole_row {
        key_idx = (0..common.len()).collect();
    }
    let rest: Vec<usize> = (0..common.len()).filter(|i| !key_idx.contains(i)).collect();
    let order: Vec<usize> = key_idx.iter().chain(rest.iter()).copied().collect();
    rep.key_columns = key_idx.iter().map(|&i| common[i].0.clone()).collect();
    rep.compared_columns = common.iter().map(|c| c.0.clone()).collect();
    Ok(Plan {
        ia: order.iter().map(|&i| common[i].1).collect(),
        ib: order.iter().map(|&i| common[i].2).collect(),
        nkeys: key_idx.len(),
        layout: order.iter().map(|&i| common[i].0.clone()).collect(),
    })
}

/// Compares two row streams. `out` receives one CSV line per difference
/// (`change,<keys…>,column,a_value,b_value`); `progress(rows_a, rows_b)` is called regularly.
pub fn compare(
    a: &mut dyn RowStream,
    b: &mut dyn RowStream,
    o: &DataDiffOptions,
    out: Option<&Path>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<DataDiffReport> {
    let started = Instant::now();
    let mut rep = DataDiffReport::default();
    let plan = plan(a.columns(), b.columns(), &o.keys, &mut rep)?;
    rep.layout = plan.layout.clone();
    let mut writer = None;
    if let Some(p) = out {
        let mut w = csv::Writer::from_path(p)
            .map_err(|e| Error::Invalid(format!("cannot write {}: {e}", p.display())))?;
        let mut head = vec!["change".to_string()];
        head.extend(plan.layout[..plan.nkeys].iter().cloned());
        head.extend(["column".into(), "a_value".into(), "b_value".into()]);
        w.write_record(&head)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        writer = Some(w);
        rep.diff_file = Some(p.display().to_string());
    }
    let mut run = Run {
        o,
        plan: &plan,
        canon: Canon { o },
        rep,
        by_col: HashMap::new(),
        out: writer,
        out_err: None,
    };
    let parts = o.partitions.max(2);
    let limit = o.memory_rows.max(1);
    let mut map: HashMap<String, Entry> = HashMap::new();
    let mut spill: Option<Spill> = None;
    let mut tick = 0u64;

    // ---- side A
    while let Some(row) = a.next_row()? {
        if cancel.load(Ordering::Relaxed) {
            run.rep.cancelled = true;
            break;
        }
        run.rep.rows_a += 1;
        tick += 1;
        if tick.is_multiple_of(20_000) {
            progress(run.rep.rows_a, run.rep.rows_b);
        }
        let vals = run.project(&row, true);
        let key = key_of(&run.canon_row(&vals[..plan.nkeys], true));
        let Some(key) = key else {
            run.rep.empty_keys_a += 1;
            run.unmatched(&vals, true);
            continue;
        };
        if let Some(sp) = spill.as_mut() {
            put_row(&mut sp.a[partition_of(&key, parts)], &vals).map_err(io)?;
            continue;
        }
        if map.contains_key(&key) {
            run.rep.duplicate_keys_a += 1;
            continue;
        }
        map.insert(
            key,
            Entry {
                vals,
                matched: false,
            },
        );
        if map.len() > limit {
            let mut sp = Spill::new(parts)?;
            for (k, e) in map.drain() {
                put_row(&mut sp.a[partition_of(&k, parts)], &e.vals).map_err(io)?;
            }
            spill = Some(sp);
            run.rep.spilled = true;
        }
    }

    // ---- side B
    if !run.rep.cancelled {
        while let Some(row) = b.next_row()? {
            if cancel.load(Ordering::Relaxed) {
                run.rep.cancelled = true;
                break;
            }
            run.rep.rows_b += 1;
            tick += 1;
            if tick.is_multiple_of(20_000) {
                progress(run.rep.rows_a, run.rep.rows_b);
            }
            let vals = run.project(&row, false);
            let Some(key) = key_of(&run.canon_row(&vals[..plan.nkeys], false)) else {
                run.rep.empty_keys_b += 1;
                run.unmatched(&vals, false);
                continue;
            };
            if let Some(sp) = spill.as_mut() {
                put_row(&mut sp.b[partition_of(&key, parts)], &vals).map_err(io)?;
                continue;
            }
            match map.get_mut(&key) {
                Some(e) => {
                    let mut entry = Entry {
                        vals: std::mem::take(&mut e.vals),
                        matched: e.matched,
                    };
                    run.matched(&mut entry, vals);
                    e.vals = entry.vals;
                    e.matched = entry.matched;
                }
                None => run.unmatched(&vals, false),
            }
        }
    }

    // ---- finish: leftovers of the in-memory map, or each spilled partition
    if let Some(mut sp) = spill {
        for w in sp.a.iter_mut().chain(sp.b.iter_mut()) {
            w.flush().map_err(io)?;
        }
        if !run.rep.cancelled {
            for i in 0..parts {
                if cancel.load(Ordering::Relaxed) {
                    run.rep.cancelled = true;
                    break;
                }
                let mut m: HashMap<String, Entry> = HashMap::new();
                let mut ra =
                    BufReader::new(File::open(sp.dir.0.join(format!("a{i}"))).map_err(io)?);
                while let Some(vals) = get_row(&mut ra).map_err(io)? {
                    let key = key_of(&run.canon_row(&vals[..plan.nkeys], true)).unwrap_or_default();
                    match m.entry(key) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            run.rep.duplicate_keys_a += 1
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(Entry {
                                vals,
                                matched: false,
                            });
                        }
                    }
                }
                let mut rb =
                    BufReader::new(File::open(sp.dir.0.join(format!("b{i}"))).map_err(io)?);
                while let Some(vals) = get_row(&mut rb).map_err(io)? {
                    let key =
                        key_of(&run.canon_row(&vals[..plan.nkeys], false)).unwrap_or_default();
                    match m.get_mut(&key) {
                        Some(e) => {
                            let mut entry = Entry {
                                vals: std::mem::take(&mut e.vals),
                                matched: e.matched,
                            };
                            run.matched(&mut entry, vals);
                            e.vals = entry.vals;
                            e.matched = entry.matched;
                        }
                        None => run.unmatched(&vals, false),
                    }
                }
                for e in m.into_values().filter(|e| !e.matched) {
                    run.unmatched(&e.vals, true);
                }
            }
        }
    } else if !run.rep.cancelled {
        for e in map.into_values().filter(|e| !e.matched) {
            run.unmatched(&e.vals, true);
        }
    }

    if let Some(w) = run.out.as_mut() {
        w.flush().map_err(io)?;
    }
    let mut rep = run.rep;
    if let Some(e) = run.out_err {
        rep.warnings.push(format!(
            "the differences file could not be fully written: {e}"
        ));
    }
    let mut cols: Vec<(String, u64)> = run.by_col.into_iter().collect();
    cols.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rep.by_column = cols;
    rep.identical =
        !rep.cancelled && rep.changed + rep.only_a + rep.only_b == 0 && rep.rows_a == rep.rows_b;
    rep.elapsed_ms = started.elapsed().as_millis() as u64;
    if rep.duplicate_keys_a + rep.duplicate_keys_b > 0 {
        rep.warnings.push(format!("the key is not unique ({} repeated key(s) in A, {} in B): only the first row of each key was compared; choose more key columns", rep.duplicate_keys_a, rep.duplicate_keys_b));
    }
    if rep.empty_keys_a + rep.empty_keys_b > 0 {
        rep.warnings.push(format!("{} row(s) in A and {} in B have an empty key and cannot be matched; they are counted as only in A / only in B", rep.empty_keys_a, rep.empty_keys_b));
    }
    if !rep.only_in_a_columns.is_empty() || !rep.only_in_b_columns.is_empty() {
        rep.warnings.push(format!(
            "columns not compared because only one side has them: A only [{}], B only [{}]",
            rep.only_in_a_columns.join(", "),
            rep.only_in_b_columns.join(", ")
        ));
    }
    progress(rep.rows_a, rep.rows_b);
    Ok(rep)
}

/// Candidate key columns from a sample: columns that are filled and distinct in every sampled row,
/// best-named first (`ID`, `_ID`, `KEY`, `CODE`, `NO`…), then pairs when no single column works.
pub fn suggest_keys(columns: &[String], sample: &[Vec<Cell>]) -> Vec<Vec<String>> {
    if sample.is_empty() {
        return vec![];
    }
    let unique = |idx: &[usize]| -> bool {
        let mut seen = HashSet::new();
        sample.iter().all(|r| {
            let parts: Option<Vec<String>> = idx
                .iter()
                .map(|&i| r.get(i).cloned().flatten().filter(|s| !s.trim().is_empty()))
                .collect();
            parts.is_some_and(|p| seen.insert(p.join("\u{1f}")))
        })
    };
    let score = |name: &str, pos: usize| -> i32 {
        let n = name.to_uppercase();
        let hint = n == "ID"
            || n.ends_with("_ID")
            || n.ends_with("ID")
            || n.ends_with("_KEY")
            || n == "KEY"
            || n.ends_with("_CODE")
            || n == "CODE"
            || n.ends_with("_NO")
            || n.ends_with("_NUMBER");
        (if hint { 100 } else { 0 }) - pos as i32
    };
    let mut singles: Vec<usize> = (0..columns.len()).filter(|&i| unique(&[i])).collect();
    singles.sort_by_key(|&i| -score(&columns[i], i));
    let mut out: Vec<Vec<String>> = singles
        .iter()
        .take(3)
        .map(|&i| vec![columns[i].clone()])
        .collect();
    if out.is_empty() {
        let n = columns.len().min(6);
        'outer: for i in 0..n {
            for j in (i + 1)..n {
                if unique(&[i, j]) {
                    out.push(vec![columns[i].clone(), columns[j].clone()]);
                    if out.len() >= 3 {
                        break 'outer;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Cell {
        Some(v.to_string())
    }
    fn rows(data: &[&[&str]]) -> Vec<Vec<Cell>> {
        data.iter()
            .map(|r| {
                r.iter()
                    .map(|c| if c == &"~" { None } else { Some(c.to_string()) })
                    .collect()
            })
            .collect()
    }
    fn run(
        a: (&[&str], &[&[&str]]),
        b: (&[&str], &[&[&str]]),
        o: &DataDiffOptions,
    ) -> DataDiffReport {
        let mut sa = VecStream::new(a.0, rows(a.1));
        let mut sb = VecStream::new(b.0, rows(b.1));
        compare(
            &mut sa,
            &mut sb,
            o,
            None,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap()
    }
    fn keyed(k: &[&str]) -> DataDiffOptions {
        DataDiffOptions {
            keys: k.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn matches_by_key_and_reports_changed_added_and_removed_rows() {
        let r = run(
            (
                &["ID", "NAME", "SALARY"],
                &[
                    &["1", "Ann", "100"],
                    &["2", "Bob", "200"],
                    &["3", "Cy", "300"],
                ],
            ),
            (
                &["ID", "NAME", "SALARY"],
                &[
                    &["3", "Cy", "300"],
                    &["1", "Ann", "150"],
                    &["4", "Dee", "400"],
                ],
            ),
            &keyed(&["ID"]),
        );
        assert_eq!(
            (r.rows_a, r.rows_b, r.same, r.changed, r.only_a, r.only_b),
            (3, 3, 1, 1, 1, 1)
        );
        assert_eq!(r.changed_samples[0].key, vec![s("1")]);
        assert_eq!(r.changed_samples[0].diffs.len(), 1);
        let d = &r.changed_samples[0].diffs[0];
        assert_eq!(
            (d.column.as_str(), d.a.clone(), d.b.clone()),
            ("SALARY", s("100"), s("150"))
        );
        assert_eq!(r.only_a_samples[0][0], s("2"));
        assert_eq!(r.only_b_samples[0][0], s("4"));
        assert_eq!(r.by_column, vec![("SALARY".to_string(), 1)]);
        assert!(!r.identical);
    }

    #[test]
    fn values_compare_by_meaning_not_spelling() {
        let r = run(
            (
                &["ID", "N", "D", "T", "E"],
                &[&["1", "1.0", "2021-05-01", "Abc ", ""]],
            ),
            (
                &["ID", "N", "D", "T", "E"],
                &[&["1", "+1", "2021-05-01 00:00:00", "Abc", "~"]],
            ),
            &keyed(&["ID"]),
        );
        assert_eq!((r.same, r.changed), (1, 0), "{:?}", r.changed_samples);
        let strict = DataDiffOptions {
            numeric: false,
            dates: false,
            trim: false,
            empty_is_null: false,
            ..keyed(&["ID"])
        };
        let r = run(
            (
                &["ID", "N", "D", "T", "E"],
                &[&["1", "1.0", "2021-05-01", "Abc ", ""]],
            ),
            (
                &["ID", "N", "D", "T", "E"],
                &[&["1", "+1", "2021-05-01 00:00:00", "Abc", "~"]],
            ),
            &strict,
        );
        assert_eq!(
            r.changed_samples[0].diffs.len(),
            4,
            "all four non-key columns differ when nothing is lenient"
        );
    }

    #[test]
    fn real_differences_are_not_hidden_by_the_leniency() {
        let r = run(
            (
                &["ID", "ZIP", "AMT", "TS", "NAME"],
                &[&["1", "01234", "10.50", "2021-05-01 10:00:00.5", "Ann"]],
            ),
            (
                &["ID", "ZIP", "AMT", "TS", "NAME"],
                &[&["1", "1234", "10.5001", "2021-05-01 10:00:00.6", "ann"]],
            ),
            &keyed(&["ID"]),
        );
        let cols: Vec<&str> = r.changed_samples[0]
            .diffs
            .iter()
            .map(|d| d.column.as_str())
            .collect();
        assert_eq!(
            cols,
            vec!["ZIP", "AMT", "TS", "NAME"],
            "leading zeros stay text; case matters by default"
        );
        let ci = DataDiffOptions {
            ignore_case: true,
            ..keyed(&["ID"])
        };
        let r = run(
            (&["ID", "NAME"], &[&["1", "Ann"]]),
            (&["ID", "NAME"], &[&["1", "ANN"]]),
            &ci,
        );
        assert_eq!(r.same, 1);
    }

    #[test]
    fn decimal_comma_files_compare_with_dot_databases() {
        let o = DataDiffOptions {
            a_decimal: ',',
            ..keyed(&["ID"])
        };
        let r = run(
            (&["ID", "AMT"], &[&["1", "1234,50"]]),
            (&["ID", "AMT"], &[&["1", "1234.5"]]),
            &o,
        );
        assert_eq!(r.same, 1);
    }

    #[test]
    fn composite_keys_and_column_names_are_matched_case_insensitively() {
        let r = run(
            (
                &["Region", "Id", "Qty"],
                &[&["EU", "1", "5"], &["US", "1", "6"]],
            ),
            (
                &["id", "REGION", "qty", "extra"],
                &[&["1", "US", "6", "x"], &["1", "EU", "9", "x"]],
            ),
            &keyed(&["region", "ID"]),
        );
        assert_eq!((r.same, r.changed), (1, 1));
        assert_eq!(r.key_columns, vec!["Region", "Id"]);
        assert_eq!(r.only_in_b_columns, vec!["extra"]);
        assert!(r.warnings.iter().any(|w| w.contains("not compared")));
    }

    #[test]
    fn whole_row_mode_reports_only_added_and_removed() {
        let r = run(
            (&["A", "B"], &[&["1", "x"], &["2", "y"]]),
            (&["A", "B"], &[&["2", "y"], &["3", "z"]]),
            &DataDiffOptions::default(),
        );
        assert!(r.whole_row);
        assert_eq!((r.same, r.changed, r.only_a, r.only_b), (1, 0, 1, 1));
    }

    #[test]
    fn repeated_keys_and_empty_keys_are_reported_not_ignored() {
        let r = run(
            (&["ID", "V"], &[&["1", "a"], &["1", "b"], &["~", "c"]]),
            (&["ID", "V"], &[&["1", "a"], &["1", "a"], &[" ", "d"]]),
            &keyed(&["ID"]),
        );
        assert_eq!(
            (
                r.duplicate_keys_a,
                r.duplicate_keys_b,
                r.empty_keys_a,
                r.empty_keys_b
            ),
            (1, 1, 1, 1)
        );
        assert_eq!((r.same, r.only_a, r.only_b), (1, 1, 1));
        assert!(
            r.warnings.iter().any(|w| w.contains("not unique"))
                && r.warnings.iter().any(|w| w.contains("empty key"))
        );
    }

    #[test]
    fn a_missing_key_or_no_common_column_is_an_error() {
        let mut a = VecStream::new(&["X"], vec![]);
        let mut b = VecStream::new(&["Y"], vec![]);
        let e = compare(
            &mut a,
            &mut b,
            &keyed(&["X"]),
            None,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("no column name in common"), "{e}");
        let mut a = VecStream::new(&["X", "Z"], vec![]);
        let mut b = VecStream::new(&["X", "Y"], vec![]);
        let e = compare(
            &mut a,
            &mut b,
            &keyed(&["Z"]),
            None,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("key column") && e.contains("not in both"), "{e}");
    }

    /// Reference implementation: the obvious in-memory comparison.
    fn naive(a: &[Vec<Cell>], b: &[Vec<Cell>]) -> (u64, u64, u64, u64) {
        let bm: HashMap<&str, &Vec<Cell>> =
            b.iter().map(|r| (r[0].as_deref().unwrap(), r)).collect();
        let am: HashSet<&str> = a.iter().map(|r| r[0].as_deref().unwrap()).collect();
        let (mut same, mut changed, mut only_a) = (0, 0, 0);
        for r in a {
            match bm.get(r[0].as_deref().unwrap()) {
                Some(x) if *x == r => same += 1,
                Some(_) => changed += 1,
                None => only_a += 1,
            }
        }
        let only_b = b
            .iter()
            .filter(|r| !am.contains(r[0].as_deref().unwrap()))
            .count() as u64;
        (same, changed, only_a, only_b)
    }

    #[test]
    fn spilling_to_disk_gives_exactly_the_same_answer_as_memory() {
        // Deterministic pseudo-random data: 20 000 rows each, ~10% changed, ~5% only on one side.
        let mut x: u64 = 12345;
        let mut next = || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32
        };
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for id in 0..20_000u32 {
            let row = vec![
                s(&id.to_string()),
                s(&format!("name{}", id % 97)),
                s(&format!("{}", id * 3)),
            ];
            let r = next() % 100;
            if r < 5 {
                a.push(row);
            } else if r < 10 {
                b.push(row);
            } else if r < 20 {
                a.push(row.clone());
                let mut c = row;
                c[2] = s("changed");
                b.push(c);
            } else {
                a.push(row.clone());
                b.push(row);
            }
        }
        let want = naive(&a, &b);
        for (label, mem) in [("memory", 1_000_000usize), ("spill", 500)] {
            let o = DataDiffOptions {
                memory_rows: mem,
                partitions: 16,
                max_samples: 5,
                ..keyed(&["C0"])
            };
            let mut sa = VecStream::new(&["C0", "C1", "C2"], a.clone());
            let mut sb = VecStream::new(&["C0", "C1", "C2"], b.clone());
            let r = compare(
                &mut sa,
                &mut sb,
                &o,
                None,
                &AtomicBool::new(false),
                &mut |_, _| {},
            )
            .unwrap();
            assert_eq!(r.spilled, label == "spill");
            assert_eq!((r.same, r.changed, r.only_a, r.only_b), want, "{label}");
            assert_eq!(r.changed_samples.len(), 5);
        }
    }

    #[test]
    fn the_differences_file_lists_every_difference_one_cell_per_line() {
        let dir = std::env::temp_dir().join(format!("ora-ide-dd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("diff.csv");
        let mut sa = VecStream::new(
            &["ID", "N", "M"],
            rows(&[&["1", "a", "x"], &["2", "b", "y"]]),
        );
        let mut sb = VecStream::new(
            &["ID", "N", "M"],
            rows(&[&["1", "z", "x"], &["3", "c", "w"]]),
        );
        let r = compare(
            &mut sa,
            &mut sb,
            &keyed(&["ID"]),
            Some(&p),
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.remove(0), "change,ID,column,a_value,b_value");
        lines.sort();
        assert_eq!(
            lines,
            vec![
                "changed,1,N,a,z",
                "only_a,2,M,y,",
                "only_a,2,N,b,",
                "only_b,3,M,,w",
                "only_b,3,N,,c"
            ]
        );
        assert_eq!(r.diff_file.as_deref(), Some(p.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelling_stops_early_and_says_so() {
        let data: Vec<Vec<Cell>> = (0..50_000).map(|i| vec![s(&i.to_string())]).collect();
        let cancel = AtomicBool::new(false);
        let mut sa = VecStream::new(&["ID"], data.clone());
        let mut sb = VecStream::new(&["ID"], data);
        let r = compare(
            &mut sa,
            &mut sb,
            &keyed(&["ID"]),
            None,
            &cancel,
            &mut |rows_a, _| {
                if rows_a > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
        assert!(r.cancelled && !r.identical && r.rows_a < 50_000);
    }

    #[test]
    fn identical_inputs_are_reported_identical() {
        let d: &[&[&str]] = &[&["1", "a"], &["2", "b"]];
        let r = run((&["ID", "V"], d), (&["ID", "V"], d), &keyed(&["ID"]));
        assert!(r.identical && r.same == 2);
    }

    #[test]
    fn key_suggestions_prefer_unique_filled_id_like_columns() {
        let cols: Vec<String> = ["NAME", "EMPLOYEE_ID", "DEPT"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let sample = rows(&[&["Ann", "10", "A"], &["Bob", "11", "A"], &["Cy", "12", "B"]]);
        let k = suggest_keys(&cols, &sample);
        assert_eq!(k[0], vec!["EMPLOYEE_ID"]);
        assert!(k.contains(&vec!["NAME".to_string()]) && !k.contains(&vec!["DEPT".to_string()]));
        let pair_cols: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let pair = rows(&[&["1", "x"], &["1", "y"], &["2", "x"]]);
        assert_eq!(
            suggest_keys(&pair_cols, &pair),
            vec![vec!["A".to_string(), "B".to_string()]]
        );
        assert!(suggest_keys(&cols, &[]).is_empty());
    }

    #[test]
    fn files_and_queries_are_streams_too() {
        let dir = std::env::temp_dir().join(format!("ora-ide-dd2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.csv");
        std::fs::write(&p, "id,name,amount\n1,Ann,\"1,200.50\"\n2,Bob,900\n").unwrap();
        let mut fa = FileStream::open(&p, &ReadOptions::default()).unwrap();
        let mut fb = VecStream::new(
            &["ID", "NAME", "AMOUNT"],
            rows(&[&["1", "Ann", "1200.5"], &["2", "Bob", "901"]]),
        );
        let r = compare(
            &mut fa,
            &mut fb,
            &keyed(&["id"]),
            None,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!((r.same, r.changed), (1, 1), "{:?}", r.changed_samples);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cargo test -p ora-core --release compare_throughput -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn compare_throughput() {
        let n = 1_000_000u32;
        let mk = |change: bool| -> Vec<Vec<Cell>> {
            (0..n)
                .map(|i| {
                    vec![
                        s(&i.to_string()),
                        s(&format!("name{}", i % 1000)),
                        s(&(if change && i % 10 == 0 { i + 1 } else { i }).to_string()),
                    ]
                })
                .collect()
        };
        for (label, mem) in [("in memory", 2_000_000usize), ("spilled to disk", 100_000)] {
            let (a, b) = (mk(false), mk(true));
            let o = DataDiffOptions {
                memory_rows: mem,
                ..keyed(&["C0"])
            };
            let mut sa = VecStream::new(&["C0", "C1", "C2"], a);
            let mut sb = VecStream::new(&["C0", "C1", "C2"], b);
            let t = Instant::now();
            let r = compare(
                &mut sa,
                &mut sb,
                &o,
                None,
                &AtomicBool::new(false),
                &mut |_, _| {},
            )
            .unwrap();
            let secs = t.elapsed().as_secs_f64();
            println!(
                "{label}: {} + {} rows in {secs:.2}s ({:.0} rows/s), spilled={}, changed={}",
                n,
                n,
                f64::from(2 * n) / secs,
                r.spilled,
                r.changed
            );
            assert_eq!(r.changed, u64::from(n / 10));
        }
    }
}
