//! Streaming export of a result set to CSV, TSV, JSON Lines, SQL INSERT scripts or
//! Excel. Rows flow cursor -> sink in batches, so memory stays flat whatever the
//! size. Files are written as `*.part` and renamed on success, so a cancelled or
//! failed export never leaves a truncated file that looks finished.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use crate::comments::quote_ident;
use crate::db::{Cell, Column, Cursor};
use crate::{Error, Result};

/// Placeholder the Oracle backend puts in cells it cannot render as text (BLOB, BFILE, ...).
pub const UNSUPPORTED_CELL: &str = "<unsupported type>";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Csv,
    Tsv,
    JsonLines,
    Xlsx,
    SqlInsert,
}

impl Format {
    fn ext(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::JsonLines => "jsonl",
            Format::Xlsx => "xlsx",
            Format::SqlInsert => "sql",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExportOptions {
    pub format: Format,
    /// Header row (CSV/TSV/Excel).
    pub header: bool,
    pub delimiter: char,
    /// How SQL NULL is written in CSV/TSV.
    pub null_text: String,
    /// UTF-8 byte-order mark so Excel opens CSV as UTF-8.
    pub bom: bool,
    /// gzip the output (not for Excel, which is already a zip).
    pub gzip: bool,
    /// Prefix `'` to text cells that start with `= + - @` so spreadsheets cannot run them as formulas.
    pub sanitize_formulas: bool,
    /// Start a new file after this many rows (`name_001.csv`, ...).
    pub split_rows: Option<u64>,
    /// Table name for INSERT scripts, already qualified, e.g. `"HR"."EMP"`.
    pub insert_table: Option<String>,
    pub rows_per_commit: u64,
    /// Add `SET DEFINE OFF;` so `&` in data is not treated as a substitution variable.
    pub sqlplus_directives: bool,
    /// Excel rows per sheet; a new sheet starts after this many.
    pub xlsx_sheet_rows: u32,
    pub batch_size: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: Format::Csv,
            header: true,
            delimiter: ',',
            null_text: String::new(),
            bom: true,
            gzip: false,
            sanitize_formulas: true,
            split_rows: None,
            insert_table: None,
            rows_per_commit: 1000,
            sqlplus_directives: true,
            xlsx_sheet_rows: 1_048_576,
            batch_size: 5_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColKind {
    Number,
    Date,
    Timestamp,
    TimestampTz,
    Text,
    Binary,
}

impl ColKind {
    pub fn from_type(t: &str) -> Self {
        let u = t.trim().to_uppercase();
        let starts = |p: &[&str]| p.iter().any(|x| u.starts_with(x));
        if u.starts_with("TIMESTAMP") {
            return if u.contains("TIME ZONE") {
                ColKind::TimestampTz
            } else {
                ColKind::Timestamp
            };
        }
        if starts(&[
            "NUMBER",
            "FLOAT",
            "BINARY_FLOAT",
            "BINARY_DOUBLE",
            "INTEGER",
            "DECIMAL",
            "NUMERIC",
            "SMALLINT",
            "REAL",
            "DOUBLE",
        ]) {
            ColKind::Number
        } else if u.starts_with("DATE") {
            ColKind::Date
        } else if starts(&["BLOB", "BFILE", "RAW", "LONG RAW"]) {
            ColKind::Binary
        } else {
            ColKind::Text
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Progress {
    pub rows: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub file_index: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ExportReport {
    pub rows: u64,
    pub bytes: u64,
    pub files: Vec<String>,
    pub elapsed_ms: u64,
    pub rows_per_sec: u64,
    pub warnings: Vec<String>,
    pub cancelled: bool,
}

// ------------------------------------------------------------------ output plumbing

struct Counting<W: Write> {
    inner: W,
    n: Arc<AtomicU64>,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let k = self.inner.write(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

enum Out {
    Plain(BufWriter<Counting<File>>),
    Gz(Box<GzEncoder<BufWriter<Counting<File>>>>),
}

impl Write for Out {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::Plain(w) => w.write(b),
            Out::Gz(w) => w.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::Plain(w) => w.flush(),
            Out::Gz(w) => w.flush(),
        }
    }
}

impl Out {
    fn open(part: &Path, gzip: bool, counter: Arc<AtomicU64>) -> Result<Self> {
        let f = BufWriter::with_capacity(
            256 * 1024,
            Counting {
                inner: File::create(part).map_err(io)?,
                n: counter,
            },
        );
        Ok(if gzip {
            Out::Gz(Box::new(GzEncoder::new(f, Compression::fast())))
        } else {
            Out::Plain(f)
        })
    }
    fn finish(self) -> Result<()> {
        match self {
            Out::Plain(mut w) => w.flush().map_err(io),
            Out::Gz(w) => (*w).finish().map_err(io)?.flush().map_err(io),
        }
    }
}

fn io(e: std::io::Error) -> Error {
    Error::Invalid(format!("file error: {e}"))
}

/// One output file. `finish` flushes and renames `*.part` to the final name.
trait Sink {
    fn begin(&mut self, cols: &[Column]) -> Result<()>;
    fn row(&mut self, cells: &[Cell]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
    /// Delete the partial file.
    fn abort(self: Box<Self>);
}

fn part_path(fin: &Path) -> PathBuf {
    let mut s = fin.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------- CSV / TSV

struct CsvSink {
    w: Option<csv::Writer<Out>>,
    kinds: Vec<ColKind>,
    opts: ExportOptions,
    part: PathBuf,
    fin: PathBuf,
}

fn formula_like(s: &str) -> bool {
    matches!(s.chars().next(), Some('=' | '+' | '-' | '@' | '\t' | '\r'))
}

impl Sink for CsvSink {
    fn begin(&mut self, cols: &[Column]) -> Result<()> {
        self.kinds = cols
            .iter()
            .map(|c| ColKind::from_type(&c.data_type))
            .collect();
        if self.opts.header {
            let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
            self.w
                .as_mut()
                .unwrap()
                .write_record(names)
                .map_err(|e| io(e.into()))?;
        }
        Ok(())
    }
    fn row(&mut self, cells: &[Cell]) -> Result<()> {
        let rec: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| match c {
                None => self.opts.null_text.clone(),
                // Numbers are never altered: a leading '-' is a sign, not a formula.
                Some(s)
                    if self.opts.sanitize_formulas
                        && self.kinds.get(i) != Some(&ColKind::Number)
                        && formula_like(s) =>
                {
                    format!("'{s}")
                }
                Some(s) => s.clone(),
            })
            .collect();
        self.w
            .as_mut()
            .unwrap()
            .write_record(rec)
            .map_err(|e| io(e.into()))
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let w = self
            .w
            .take()
            .unwrap()
            .into_inner()
            .map_err(|e| io(e.into_error()))?;
        w.finish()?;
        fs::rename(&self.part, &self.fin).map_err(io)
    }
    fn abort(mut self: Box<Self>) {
        drop(self.w.take());
        let _ = fs::remove_file(&self.part);
    }
}

// ------------------------------------------------------------------------ JSON Lines

struct JsonSink {
    out: Option<Out>,
    names: Vec<String>,
    kinds: Vec<ColKind>,
    part: PathBuf,
    fin: PathBuf,
}

/// Oracle prints 0.5 as `.5`; JSON needs a leading digit.
fn json_number(s: &str) -> Option<String> {
    let t = s.trim();
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", t.strip_prefix('+').unwrap_or(t)),
    };
    let rest = if rest.starts_with('.') {
        format!("0{rest}")
    } else {
        rest.to_string()
    };
    let digits = rest.chars().filter(|c| c.is_ascii_digit()).count();
    let valid = !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
        && rest.parse::<f64>().is_ok()
        && !rest.ends_with('.');
    // beyond 15 significant digits a double (JavaScript, Excel) would silently round the value
    (valid && digits <= 15).then(|| format!("{sign}{rest}"))
}

impl Sink for JsonSink {
    fn begin(&mut self, cols: &[Column]) -> Result<()> {
        self.names = cols.iter().map(|c| c.name.clone()).collect();
        self.kinds = cols
            .iter()
            .map(|c| ColKind::from_type(&c.data_type))
            .collect();
        Ok(())
    }
    fn row(&mut self, cells: &[Cell]) -> Result<()> {
        let mut line = String::from("{");
        for (i, c) in cells.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            line.push_str(&serde_json::to_string(&self.names[i]).unwrap());
            line.push(':');
            match c {
                None => line.push_str("null"),
                Some(s) => match (self.kinds[i], json_number(s)) {
                    (ColKind::Number, Some(n)) => line.push_str(&n),
                    _ => line.push_str(&serde_json::to_string(s).unwrap()),
                },
            }
        }
        line.push_str("}\n");
        self.out
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .map_err(io)
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        self.out.take().unwrap().finish()?;
        fs::rename(&self.part, &self.fin).map_err(io)
    }
    fn abort(mut self: Box<Self>) {
        drop(self.out.take());
        let _ = fs::remove_file(&self.part);
    }
}

// ----------------------------------------------------------------------- SQL INSERT

struct SqlSink {
    out: Option<Out>,
    insert_head: String,
    kinds: Vec<ColKind>,
    opts: ExportOptions,
    rows: u64,
    part: PathBuf,
    fin: PathBuf,
}

/// Longest literal piece: Oracle string literals are limited to 4000 bytes.
const LITERAL_CHUNK_BYTES: usize = 3900;

pub fn sql_string_literal(s: &str) -> String {
    let q = |x: &str| format!("'{}'", x.replace('\'', "''"));
    if s.len() <= LITERAL_CHUNK_BYTES {
        return q(s);
    }
    // long text becomes TO_CLOB('..') || TO_CLOB('..'), split on char boundaries
    let mut parts = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if cur.len() + ch.len_utf8() > LITERAL_CHUNK_BYTES {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    parts.push(cur);
    parts
        .iter()
        .map(|p| format!("TO_CLOB({})", q(p)))
        .collect::<Vec<_>>()
        .join("\n||")
}

fn matches_ts(s: &str, tz: bool) -> bool {
    // YYYY-MM-DD HH24:MI:SS[.ffffff][ +HH:MM]
    let b = s.as_bytes();
    if b.len() < 19
        || !(b[4] == b'-' && b[7] == b'-' && b[10] == b' ' && b[13] == b':' && b[16] == b':')
    {
        return false;
    }
    let digits_ok = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
        .iter()
        .all(|&i| b[i].is_ascii_digit());
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.chars().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        rest = &r[n..];
    }
    let tail_ok = if tz {
        let t = rest.trim_start();
        t.len() == 6 && matches!(t.as_bytes()[0], b'+' | b'-') && t.as_bytes()[3] == b':'
    } else {
        rest.is_empty()
    };
    digits_ok && tail_ok
}

fn sql_literal(cell: &Cell, kind: ColKind, odd: &mut u64) -> String {
    let Some(s) = cell else { return "NULL".into() };
    match kind {
        ColKind::Number if s.trim().parse::<f64>().is_ok() => s.trim().to_string(),
        ColKind::Date if matches_ts(s, false) && s.len() == 19 => {
            format!("TO_DATE('{s}','YYYY-MM-DD HH24:MI:SS')")
        }
        ColKind::Timestamp if matches_ts(s, false) => {
            format!("TO_TIMESTAMP('{s}','YYYY-MM-DD HH24:MI:SS.FF6')")
        }
        ColKind::TimestampTz if matches_ts(s, true) => {
            format!("TO_TIMESTAMP_TZ('{s}','YYYY-MM-DD HH24:MI:SS.FF6 TZH:TZM')")
        }
        ColKind::Binary => "NULL".into(),
        ColKind::Number | ColKind::Date | ColKind::Timestamp | ColKind::TimestampTz => {
            *odd += 1; // not in the expected format: keep the value, as text
            sql_string_literal(s)
        }
        ColKind::Text => sql_string_literal(s),
    }
}

impl Sink for SqlSink {
    fn begin(&mut self, cols: &[Column]) -> Result<()> {
        self.kinds = cols
            .iter()
            .map(|c| ColKind::from_type(&c.data_type))
            .collect();
        let table = self
            .opts
            .insert_table
            .clone()
            .unwrap_or_else(|| "\"EXPORT_TABLE\"".into());
        let names: Vec<String> = cols
            .iter()
            .map(|c| quote_ident(&c.name))
            .collect::<Result<_>>()?;
        self.insert_head = format!("INSERT INTO {table} ({}) VALUES (", names.join(", "));
        let out = self.out.as_mut().unwrap();
        writeln!(out, "-- exported by Ora IDE").map_err(io)?;
        if self.opts.sqlplus_directives {
            writeln!(out, "SET DEFINE OFF;\nSET SQLBLANKLINES ON;").map_err(io)?;
        }
        Ok(())
    }
    fn row(&mut self, cells: &[Cell]) -> Result<()> {
        let mut odd = 0;
        let vals: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| sql_literal(c, self.kinds[i], &mut odd))
            .collect();
        let out = self.out.as_mut().unwrap();
        writeln!(out, "{}{});", self.insert_head, vals.join(", ")).map_err(io)?;
        self.rows += 1;
        if self.opts.rows_per_commit > 0 && self.rows.is_multiple_of(self.opts.rows_per_commit) {
            writeln!(out, "COMMIT;").map_err(io)?;
        }
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let mut out = self.out.take().unwrap();
        writeln!(out, "COMMIT;").map_err(io)?;
        out.finish()?;
        fs::rename(&self.part, &self.fin).map_err(io)
    }
    fn abort(mut self: Box<Self>) {
        drop(self.out.take());
        let _ = fs::remove_file(&self.part);
    }
}

// ---------------------------------------------------------------------------- Excel

struct XlsxSink {
    wb: Option<rust_xlsxwriter::Workbook>,
    kinds: Vec<ColKind>,
    header: Vec<String>,
    sheet: usize,
    row_in_sheet: u32,
    sheet_rows: u32,
    want_header: bool,
    datefmt: rust_xlsxwriter::Format,
    widths: Vec<f64>,
    part: PathBuf,
    fin: PathBuf,
}

fn parse_ts(s: &str) -> Option<(u16, u8, u8, u8, u8, f64)> {
    if !matches_ts(s, false) {
        return None;
    }
    let n = |a: usize, b: usize| s[a..b].parse::<u32>().ok();
    let secs: f64 = s[17..].parse().ok()?;
    Some((
        n(0, 4)? as u16,
        n(5, 7)? as u8,
        n(8, 10)? as u8,
        n(11, 13)? as u8,
        n(14, 16)? as u8,
        secs,
    ))
}

impl XlsxSink {
    fn new_sheet(&mut self) -> Result<()> {
        let wb = self.wb.as_mut().unwrap();
        let ws = wb.add_worksheet_with_constant_memory();
        let x = |e: rust_xlsxwriter::XlsxError| Error::Invalid(format!("excel: {e}"));
        self.sheet += 1;
        ws.set_name(format!("Data {}", self.sheet)).map_err(x)?;
        self.row_in_sheet = 0;
        if self.want_header {
            let bold = rust_xlsxwriter::Format::new().set_bold();
            for (c, h) in self.header.iter().enumerate() {
                ws.write_string_with_format(0, c as u16, h, &bold)
                    .map_err(x)?;
                ws.set_column_width(c as u16, self.widths[c].clamp(8.0, 50.0))
                    .map_err(x)?;
            }
            ws.set_freeze_panes(1, 0).map_err(x)?;
            self.row_in_sheet = 1;
        }
        Ok(())
    }
}

impl Sink for XlsxSink {
    fn begin(&mut self, cols: &[Column]) -> Result<()> {
        self.kinds = cols
            .iter()
            .map(|c| ColKind::from_type(&c.data_type))
            .collect();
        self.header = cols.iter().map(|c| c.name.clone()).collect();
        self.widths = cols
            .iter()
            .map(|c| c.name.chars().count() as f64 + 3.0)
            .collect();
        self.new_sheet()
    }
    fn row(&mut self, cells: &[Cell]) -> Result<()> {
        if self.row_in_sheet >= self.sheet_rows {
            self.new_sheet()?;
        }
        let x = |e: rust_xlsxwriter::XlsxError| Error::Invalid(format!("excel: {e}"));
        let wb = self.wb.as_mut().unwrap();
        let ws = wb.worksheet_from_index(self.sheet - 1).map_err(x)?;
        let r = self.row_in_sheet;
        for (c, cell) in cells.iter().enumerate() {
            let Some(s) = cell else { continue };
            let col = c as u16;
            // Text is always written as a string: Excel never evaluates it as a formula.
            match self.kinds[c] {
                ColKind::Number if json_number(s).is_some() => {
                    ws.write_number(r, col, s.trim().parse::<f64>().unwrap_or(0.0))
                        .map_err(x)?;
                }
                ColKind::Date | ColKind::Timestamp => {
                    if let Some((y, mo, d, h, mi, sec)) = parse_ts(s) {
                        let dt = rust_xlsxwriter::ExcelDateTime::from_ymd(y, mo, d)
                            .and_then(|d| d.and_hms(h as u16, mi, sec as u16));
                        match dt {
                            Ok(dt) => {
                                ws.write_datetime_with_format(r, col, &dt, &self.datefmt)
                                    .map_err(x)?;
                            }
                            Err(_) => {
                                ws.write_string(r, col, s).map_err(x)?;
                            }
                        }
                    } else {
                        ws.write_string(r, col, s).map_err(x)?;
                    }
                }
                _ => {
                    ws.write_string(r, col, s).map_err(x)?;
                }
            }
            if r < 200 {
                self.widths[c] = self.widths[c].max(s.chars().count() as f64 + 2.0);
            }
        }
        self.row_in_sheet += 1;
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let mut wb = self.wb.take().unwrap();
        wb.save(&self.part)
            .map_err(|e| Error::Invalid(format!("excel: {e}")))?;
        fs::rename(&self.part, &self.fin).map_err(io)
    }
    fn abort(mut self: Box<Self>) {
        drop(self.wb.take());
        let _ = fs::remove_file(&self.part);
    }
}

// ---------------------------------------------------------------------------- driver

fn make_sink(fin: &Path, opts: &ExportOptions, counter: Arc<AtomicU64>) -> Result<Box<dyn Sink>> {
    let part = part_path(fin);
    let fin = fin.to_path_buf();
    Ok(match opts.format {
        Format::Csv | Format::Tsv => {
            let mut out = Out::open(&part, opts.gzip, counter)?;
            if opts.bom {
                out.write_all(&[0xEF, 0xBB, 0xBF]).map_err(io)?;
            }
            let delim = if opts.format == Format::Tsv {
                b'\t'
            } else {
                opts.delimiter as u8
            };
            let w = csv::WriterBuilder::new()
                .delimiter(delim)
                .terminator(csv::Terminator::CRLF)
                .from_writer(out);
            Box::new(CsvSink {
                w: Some(w),
                kinds: vec![],
                opts: opts.clone(),
                part,
                fin,
            })
        }
        Format::JsonLines => Box::new(JsonSink {
            out: Some(Out::open(&part, opts.gzip, counter)?),
            names: vec![],
            kinds: vec![],
            part,
            fin,
        }),
        Format::SqlInsert => Box::new(SqlSink {
            out: Some(Out::open(&part, opts.gzip, counter)?),
            insert_head: String::new(),
            kinds: vec![],
            opts: opts.clone(),
            rows: 0,
            part,
            fin,
        }),
        Format::Xlsx => {
            // Excel's writer owns its file; the counter is updated from the finished size in `export`.
            let _ = counter;
            Box::new(XlsxSink {
                wb: Some(rust_xlsxwriter::Workbook::new()),
                kinds: vec![],
                header: vec![],
                sheet: 0,
                row_in_sheet: 0,
                sheet_rows: opts.xlsx_sheet_rows.max(2),
                want_header: opts.header,
                datefmt: rust_xlsxwriter::Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
                widths: vec![],
                part,
                fin,
            })
        }
    })
}

/// The file an export to `base` will create first (`name_001.ext` when splitting).
pub fn first_output_path(base: &Path, opts: &ExportOptions) -> PathBuf {
    file_name(base, opts, opts.split_rows.filter(|n| *n > 0).map(|_| 1))
}

fn file_name(base: &Path, opts: &ExportOptions, index: Option<usize>) -> PathBuf {
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".into());
    let mut name = match index {
        Some(i) => format!("{stem}_{i:03}.{}", opts.format.ext()),
        None => format!("{stem}.{}", opts.format.ext()),
    };
    if opts.gzip && opts.format != Format::Xlsx {
        name.push_str(".gz");
    }
    base.with_file_name(name)
}

/// Stream `cursor` into files next to `base` (the extension is chosen from the format).
/// Returns early with `cancelled: true` (and no files left behind) when `cancel` is set.
pub fn export(
    cursor: &mut dyn Cursor,
    base: &Path,
    opts: &ExportOptions,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(&Progress),
) -> Result<ExportReport> {
    let cols = cursor.columns().to_vec();
    let kinds: Vec<ColKind> = cols
        .iter()
        .map(|c| ColKind::from_type(&c.data_type))
        .collect();
    let counter = Arc::new(AtomicU64::new(0));
    let t0 = Instant::now();
    let split = opts.split_rows.filter(|n| *n > 0);

    let mut sink: Option<Box<dyn Sink>> = None;
    let mut finished: Vec<PathBuf> = Vec::new();
    let mut current: Option<PathBuf> = None;
    let mut rows: u64 = 0;
    let mut rows_in_file: u64 = 0;
    let mut unsupported = vec![0u64; cols.len()];
    let mut last_report = Instant::now();

    let abort_all = |sink: &mut Option<Box<dyn Sink>>, finished: &[PathBuf]| {
        if let Some(s) = sink.take() {
            s.abort();
        }
        for f in finished {
            let _ = fs::remove_file(f);
        }
    };

    let mut run = || -> Result<bool> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(true);
            }
            let batch = cursor.next_batch(opts.batch_size.max(1))?;
            if batch.is_empty() {
                return Ok(false);
            }
            for mut row in batch {
                if sink.is_none() {
                    let fin = file_name(base, opts, split.map(|_| finished.len() + 1));
                    let mut s = make_sink(&fin, opts, counter.clone())?;
                    s.begin(&cols)?;
                    sink = Some(s);
                    current = Some(fin);
                    rows_in_file = 0;
                }
                for (i, c) in row.iter_mut().enumerate() {
                    if kinds.get(i) == Some(&ColKind::Binary)
                        || c.as_deref() == Some(UNSUPPORTED_CELL)
                    {
                        if c.is_some() {
                            unsupported[i] += 1;
                        }
                        *c = None;
                    }
                }
                sink.as_mut().unwrap().row(&row)?;
                rows += 1;
                rows_in_file += 1;
                if split.is_some_and(|n| rows_in_file >= n) {
                    sink.take().unwrap().finish()?;
                    finished.push(current.take().unwrap());
                }
            }
            if last_report.elapsed().as_millis() >= 250 {
                last_report = Instant::now();
                on_progress(&Progress {
                    rows,
                    bytes: counter.load(Ordering::Relaxed),
                    elapsed_ms: t0.elapsed().as_millis() as u64,
                    file_index: finished.len() + 1,
                });
            }
        }
    };

    let outcome = run();
    match outcome {
        Err(e) => {
            abort_all(&mut sink, &finished);
            Err(e)
        }
        Ok(true) => {
            abort_all(&mut sink, &finished);
            Ok(ExportReport {
                rows,
                cancelled: true,
                elapsed_ms: t0.elapsed().as_millis() as u64,
                ..Default::default()
            })
        }
        Ok(false) => {
            // an export with no rows still produces a file holding the header
            if sink.is_none() && finished.is_empty() {
                let fin = file_name(base, opts, split.map(|_| 1));
                let mut s = make_sink(&fin, opts, counter.clone())?;
                s.begin(&cols)?;
                sink = Some(s);
                current = Some(fin);
            }
            if let Some(s) = sink.take() {
                s.finish()?;
                finished.push(current.take().unwrap());
            }
            let mut bytes = counter.load(Ordering::Relaxed);
            if opts.format == Format::Xlsx {
                bytes = finished
                    .iter()
                    .filter_map(|f| fs::metadata(f).ok())
                    .map(|m| m.len())
                    .sum();
            }
            let mut warnings = Vec::new();
            for (i, n) in unsupported.iter().enumerate() {
                if *n > 0 || kinds[i] == ColKind::Binary {
                    warnings.push(format!("Column {} ({}): {} value(s) could not be exported (binary/LOB) and were written as NULL.", cols[i].name, cols[i].data_type, n));
                }
            }
            let elapsed_ms = t0.elapsed().as_millis() as u64;
            let report = ExportReport {
                rows,
                bytes,
                files: finished
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect(),
                elapsed_ms,
                rows_per_sec: (rows * 1000).checked_div(elapsed_ms).unwrap_or(rows),
                warnings,
                cancelled: false,
            };
            on_progress(&Progress {
                rows,
                bytes,
                elapsed_ms,
                file_index: finished.len(),
            });
            Ok(report)
        }
    }
}

// --------------------------------------------------------------------- recommendation

pub const EXCEL_MAX_ROWS: u64 = 1_048_576;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Open in Excel / hand to a colleague.
    Spreadsheet,
    /// Load into another Oracle database.
    LoadOracle,
    /// Exchange with other tools or scripts.
    Exchange,
    /// Keep a copy.
    Archive,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Good,
    Caution,
    Avoid,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alternative {
    pub format: Format,
    pub verdict: Verdict,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    pub format: Format,
    pub gzip: bool,
    pub split_rows: Option<u64>,
    pub reasons: Vec<String>,
    pub alternatives: Vec<Alternative>,
    /// A better tool outside this app for very large Oracle-to-Oracle moves.
    pub tip: Option<String>,
}

fn n(rows: u64) -> String {
    let s = rows.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Pick a format for `est_rows` (unknown for ad-hoc queries) and a purpose.
pub fn recommend(est_rows: Option<u64>, purpose: Purpose) -> Recommendation {
    let rows = est_rows;
    let big = rows.is_some_and(|r| r > 1_000_000);
    let huge = rows.is_some_and(|r| r > 20_000_000);
    let mut reasons = Vec::new();
    let unknown_note = "Row count unknown (ad-hoc query): streaming formats are the safe choice.";

    let alternatives = {
        let r = rows;
        let xlsx = match r {
            Some(r) if r > EXCEL_MAX_ROWS => (Verdict::Avoid, format!("Excel holds at most {} rows per sheet; {} rows would spill over many sheets and be very slow to open.", n(EXCEL_MAX_ROWS), n(r))),
            Some(r) if r > 100_000 => (Verdict::Caution, format!("{} rows will make a heavy workbook that is slow to open; CSV opens faster.", n(r))),
            Some(_) => (Verdict::Good, "Typed numbers and dates, header frozen, opens directly in Excel.".into()),
            None => (Verdict::Caution, "Row count unknown; Excel is only suitable up to about 100,000 rows.".into()),
        };
        let sql = match r {
            Some(r) if r > 200_000 => (Verdict::Avoid, format!("An INSERT script for {} rows would take very long to run: the database parses every statement.", n(r))),
            Some(r) if r > 10_000 => (Verdict::Caution, "Works, but a script this size loads at only a few thousand rows per second.".into()),
            Some(_) => (Verdict::Good, "Self-contained and easy to review; fine at this size.".into()),
            None => (Verdict::Caution, "Row count unknown; only use INSERT scripts for small sets.".into()),
        };
        vec![
            Alternative { format: Format::Csv, verdict: Verdict::Good, note: "Streams in constant memory at any size and every tool reads it. Add gzip to cut the size 5-10x.".into() },
            Alternative { format: Format::Xlsx, verdict: xlsx.0, note: xlsx.1 },
            Alternative { format: Format::SqlInsert, verdict: sql.0, note: sql.1 },
            Alternative { format: Format::JsonLines, verdict: Verdict::Good, note: "One JSON object per line; streams like CSV and keeps types for scripts and APIs.".into() },
        ]
    };

    let (format, gzip, split_rows) = match purpose {
        Purpose::Spreadsheet => match rows {
            Some(r) if r <= 100_000 => {
                reasons.push(format!("{} rows fit comfortably in one Excel sheet.", n(r)));
                (Format::Xlsx, false, None)
            }
            Some(r) if r <= EXCEL_MAX_ROWS => {
                reasons.push(format!(
                    "{} rows is heavy for an .xlsx file; CSV opens in Excel much faster.",
                    n(r)
                ));
                (Format::Csv, false, None)
            }
            Some(r) => {
                reasons.push(format!(
                    "{} rows exceed Excel's limit of {} per sheet.",
                    n(r),
                    n(EXCEL_MAX_ROWS)
                ));
                reasons.push("The data is split into files of 1,000,000 rows so each one opens in Excel; for analysis at this size load it into a database or Power Query instead.".into());
                (Format::Csv, true, Some(1_000_000))
            }
            None => {
                reasons.push(unknown_note.into());
                (Format::Csv, false, None)
            }
        },
        Purpose::LoadOracle => match rows {
            Some(r) if r <= 10_000 => {
                reasons.push(format!(
                    "{} rows: an INSERT script is self-contained and easy to review.",
                    n(r)
                ));
                (Format::SqlInsert, false, None)
            }
            _ => {
                reasons.push("CSV loads far faster than INSERT statements (array-bound batches, no per-row parsing).".into());
                if big {
                    reasons.push("gzip keeps the file small; large files are split so a failure only costs one part.".into());
                }
                (Format::Csv, big, if big { Some(5_000_000) } else { None })
            }
        },
        Purpose::Exchange => {
            reasons.push("CSV is understood by every tool; UTF-8 with a header row.".into());
            (
                Format::Csv,
                big,
                if rows.is_some_and(|r| r > 5_000_000) {
                    Some(5_000_000)
                } else {
                    None
                },
            )
        }
        Purpose::Archive => {
            reasons.push(
                "CSV is a plain-text, future-proof archive; gzip shrinks it substantially.".into(),
            );
            (
                Format::Csv,
                true,
                if rows.is_some_and(|r| r > 5_000_000) {
                    Some(5_000_000)
                } else {
                    None
                },
            )
        }
    };
    if rows.is_none() && purpose != Purpose::Spreadsheet {
        reasons.push(unknown_note.into());
    }
    let tip = (purpose == Purpose::LoadOracle && (huge || rows.is_some_and(|r| r > 5_000_000))).then(|| {
        "For tens of millions of rows between Oracle databases, Data Pump (expdp/impdp) or a database link with INSERT /*+ APPEND */ SELECT runs inside the database and is much faster than any client-side export.".to_string()
    });
    Recommendation {
        format,
        gzip,
        split_rows,
        reasons,
        alternatives,
        tip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Row;
    use std::sync::atomic::AtomicUsize;

    struct VecCursor {
        cols: Vec<Column>,
        rows: std::vec::IntoIter<Row>,
        calls: usize,
    }
    impl Cursor for VecCursor {
        fn columns(&self) -> &[Column] {
            &self.cols
        }
        fn next_batch(&mut self, max: usize) -> Result<Vec<Row>> {
            self.calls += 1;
            Ok(self.rows.by_ref().take(max).collect())
        }
    }

    fn col(n: &str, t: &str) -> Column {
        Column {
            name: n.into(),
            data_type: t.into(),
            nullable: true,
        }
    }
    fn cur(cols: Vec<Column>, rows: Vec<Vec<Option<&str>>>) -> VecCursor {
        VecCursor {
            cols,
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(|c| c.map(String::from)).collect())
                .collect::<Vec<Row>>()
                .into_iter(),
            calls: 0,
        }
    }

    static DIR_N: AtomicUsize = AtomicUsize::new(0);
    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ora-ide-exp-{}-{}",
            std::process::id(),
            DIR_N.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn run(c: &mut VecCursor, dir: &Path, opts: ExportOptions) -> ExportReport {
        export(
            c,
            &dir.join("out"),
            &opts,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap()
    }
    fn text(p: &str) -> String {
        String::from_utf8(fs::read(p).unwrap()).unwrap()
    }
    fn no_bom() -> ExportOptions {
        ExportOptions {
            bom: false,
            ..Default::default()
        }
    }

    fn sample() -> VecCursor {
        cur(
            vec![
                col("ID", "NUMBER(10)"),
                col("NAME", "VARCHAR2(30)"),
                col("AMOUNT", "NUMBER(10,2)"),
            ],
            vec![
                vec![Some("1"), Some("Ann, \"the\" boss"), Some("-5.50")],
                vec![Some("2"), None, Some(".5")],
                vec![Some("3"), Some("line1\nline2"), None],
            ],
        )
    }

    #[test]
    fn csv_quotes_header_crlf_and_atomic_naming() {
        let d = tmp();
        let r = run(&mut sample(), &d, no_bom());
        assert_eq!((r.rows, r.files.len(), r.cancelled), (3, 1, false));
        assert!(r.files[0].ends_with("out.csv"));
        assert_eq!(text(&r.files[0]), "ID,NAME,AMOUNT\r\n1,\"Ann, \"\"the\"\" boss\",-5.50\r\n2,,.5\r\n3,\"line1\nline2\",\r\n");
        assert!(
            !d.join("out.csv.part").exists(),
            "no .part file left behind"
        );
    }

    #[test]
    fn csv_options_bom_delimiter_null_text_no_header_tsv() {
        let d = tmp();
        let r = run(
            &mut sample(),
            &d,
            ExportOptions {
                delimiter: ';',
                null_text: "NULL".into(),
                header: false,
                ..Default::default()
            },
        );
        let b = fs::read(&r.files[0]).unwrap();
        assert_eq!(&b[..3], &[0xEF, 0xBB, 0xBF], "UTF-8 BOM for Excel");
        assert!(String::from_utf8(b[3..].to_vec())
            .unwrap()
            .starts_with("1;\"Ann, \"\"the\"\" boss\";-5.50\r\n2;NULL;.5"));
        let r = run(
            &mut sample(),
            &d,
            ExportOptions {
                format: Format::Tsv,
                bom: false,
                ..Default::default()
            },
        );
        assert!(
            r.files[0].ends_with("out.tsv")
                && text(&r.files[0]).starts_with("ID\tNAME\tAMOUNT\r\n1\t")
        );
    }

    #[test]
    fn formula_injection_is_neutralised_but_negative_numbers_are_not() {
        let d = tmp();
        let mut c = cur(
            vec![col("N", "NUMBER"), col("T", "VARCHAR2(50)")],
            vec![
                vec![Some("-5"), Some("=HYPERLINK(\"http://x\")")],
                vec![Some("7"), Some("+1 555")],
                vec![Some("8"), Some("@SUM(A1)")],
                vec![Some("9"), Some("plain")],
            ],
        );
        let r = run(&mut c, &d, no_bom());
        let t = text(&r.files[0]);
        assert!(t.contains("-5,\"'=HYPERLINK(\"\"http://x\"\")\""), "{t}");
        assert!(
            t.contains("7,'+1 555") && t.contains("8,'@SUM(A1)") && t.contains("9,plain"),
            "{t}"
        );
        let r = run(
            &mut cur(vec![col("T", "VARCHAR2(5)")], vec![vec![Some("=1+1")]]),
            &d,
            ExportOptions {
                sanitize_formulas: false,
                bom: false,
                ..Default::default()
            },
        );
        assert!(text(&r.files[0]).contains("=1+1") && !text(&r.files[0]).contains("'=1+1"));
    }

    #[test]
    fn gzip_round_trips_and_is_smaller() {
        let d = tmp();
        let rows: Vec<Vec<Option<&str>>> = (0..2000)
            .map(|_| {
                vec![
                    Some("1"),
                    Some("the same text again and again"),
                    Some("2.5"),
                ]
            })
            .collect();
        let plain = run(
            &mut cur(
                vec![
                    col("A", "NUMBER"),
                    col("B", "VARCHAR2(40)"),
                    col("C", "NUMBER"),
                ],
                rows.clone(),
            ),
            &d,
            no_bom(),
        );
        let gz = run(
            &mut cur(
                vec![
                    col("A", "NUMBER"),
                    col("B", "VARCHAR2(40)"),
                    col("C", "NUMBER"),
                ],
                rows,
            ),
            &d,
            ExportOptions {
                gzip: true,
                bom: false,
                ..Default::default()
            },
        );
        assert!(gz.files[0].ends_with("out.csv.gz"));
        assert!(
            gz.bytes < plain.bytes / 4,
            "{} vs {}",
            gz.bytes,
            plain.bytes
        );
        let mut s = String::new();
        std::io::Read::read_to_string(
            &mut flate2::read::GzDecoder::new(File::open(&gz.files[0]).unwrap()),
            &mut s,
        )
        .unwrap();
        assert_eq!(s, fs::read_to_string(d.join("out.csv")).unwrap());
    }

    #[test]
    fn large_exports_split_into_numbered_files() {
        let d = tmp();
        let rows: Vec<Vec<Option<&str>>> = (0..25).map(|_| vec![Some("x")]).collect();
        let r = run(
            &mut cur(vec![col("A", "VARCHAR2(1)")], rows),
            &d,
            ExportOptions {
                split_rows: Some(10),
                bom: false,
                ..Default::default()
            },
        );
        assert_eq!(r.files.len(), 3);
        assert!(r.files[0].ends_with("out_001.csv") && r.files[2].ends_with("out_003.csv"));
        let lines: Vec<usize> = r.files.iter().map(|f| text(f).lines().count()).collect();
        assert_eq!(
            lines,
            vec![11, 11, 6],
            "every part repeats the header: 10+1, 10+1, 5+1"
        );
    }

    #[test]
    fn empty_result_still_writes_the_header() {
        let d = tmp();
        let r = run(
            &mut cur(vec![col("A", "NUMBER"), col("B", "DATE")], vec![]),
            &d,
            no_bom(),
        );
        assert_eq!((r.rows, text(&r.files[0]).as_str()), (0, "A,B\r\n"));
    }

    #[test]
    fn jsonl_keeps_types_but_never_rounds_big_numbers() {
        let d = tmp();
        let mut c = cur(
            vec![
                col("ID", "NUMBER(30)"),
                col("AMT", "NUMBER(10,2)"),
                col("NAME", "VARCHAR2(9)"),
            ],
            vec![
                vec![Some("123456789012345678901"), Some(".5"), Some("a\"b")],
                vec![Some("42"), Some("-.25"), None],
            ],
        );
        let r = run(
            &mut c,
            &d,
            ExportOptions {
                format: Format::JsonLines,
                ..Default::default()
            },
        );
        let lines: Vec<String> = text(&r.files[0]).lines().map(String::from).collect();
        assert_eq!(
            lines[0], r#"{"ID":"123456789012345678901","AMT":0.5,"NAME":"a\"b"}"#,
            "21-digit id stays a string"
        );
        assert_eq!(lines[1], r#"{"ID":42,"AMT":-0.25,"NAME":null}"#);
        for l in &lines {
            serde_json::from_str::<serde_json::Value>(l).unwrap();
        }
    }

    #[test]
    fn sql_insert_script_is_safe_and_typed() {
        let d = tmp();
        let long = "é".repeat(3000); // 6000 bytes: must be chunked
        let mut c = cur(
            vec![
                col("ID", "NUMBER"),
                col("NAME", "VARCHAR2(4000)"),
                col("D", "DATE"),
                col("TS", "TIMESTAMP(6)"),
                col("TZ", "TIMESTAMP(6) WITH TIME ZONE"),
                col("B", "BLOB"),
            ],
            vec![
                vec![
                    Some("1"),
                    Some("O'Brien & Sons"),
                    Some("2020-01-31 10:20:30"),
                    Some("2020-01-31 10:20:30.123456"),
                    Some("2020-01-31 10:20:30.000000 +05:30"),
                    Some(UNSUPPORTED_CELL),
                ],
                vec![Some("2"), Some(&long), Some("garbage"), None, None, None],
            ],
        );
        let r = run(
            &mut c,
            &d,
            ExportOptions {
                format: Format::SqlInsert,
                insert_table: Some("\"HR\".\"EMP\"".into()),
                rows_per_commit: 1,
                ..Default::default()
            },
        );
        let s = text(&r.files[0]);
        assert!(
            s.contains("SET DEFINE OFF;"),
            "so & in data is not a substitution variable"
        );
        assert!(s.contains(r#"INSERT INTO "HR"."EMP" ("ID", "NAME", "D", "TS", "TZ", "B") VALUES (1, 'O''Brien & Sons', TO_DATE('2020-01-31 10:20:30','YYYY-MM-DD HH24:MI:SS'), TO_TIMESTAMP('2020-01-31 10:20:30.123456','YYYY-MM-DD HH24:MI:SS.FF6'), TO_TIMESTAMP_TZ('2020-01-31 10:20:30.000000 +05:30','YYYY-MM-DD HH24:MI:SS.FF6 TZH:TZM'), NULL);"#), "{s}");
        assert!(
            s.contains("TO_CLOB(") && s.contains("\n||TO_CLOB("),
            "long text is chunked"
        );
        for piece in s.split("TO_CLOB('").skip(1) {
            assert!(
                piece.split("')").next().unwrap().len() <= LITERAL_CHUNK_BYTES,
                "every literal stays under 4000 bytes"
            );
        }
        assert!(
            s.contains("'garbage'"),
            "a date not in the expected format is kept, as text"
        );
        assert_eq!(
            s.matches("COMMIT;").count(),
            3,
            "after each row (every 1) and at the end"
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("BLOB")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn excel_has_typed_cells_and_rolls_over_to_a_new_sheet() {
        use calamine::{open_workbook, Data, Reader, Xlsx};
        let d = tmp();
        let mut c = cur(
            vec![
                col("ID", "NUMBER(30)"),
                col("NAME", "VARCHAR2(20)"),
                col("D", "DATE"),
            ],
            vec![
                vec![Some("1"), Some("=1+1"), Some("2020-02-29 13:45:00")],
                vec![Some("12345678901234567890"), Some("b"), None],
                vec![Some("3"), Some("c"), Some("2021-01-01 00:00:00")],
                vec![Some("4"), Some("d"), None],
                vec![Some("5"), Some("e"), None],
            ],
        );
        let r = run(
            &mut c,
            &d,
            ExportOptions {
                format: Format::Xlsx,
                xlsx_sheet_rows: 4,
                ..Default::default()
            },
        );
        assert!(r.files[0].ends_with("out.xlsx") && r.bytes > 0);
        let mut wb: Xlsx<_> = open_workbook(&r.files[0]).unwrap();
        assert_eq!(
            wb.sheet_names(),
            vec!["Data 1", "Data 2"],
            "1,048,576-row limit is handled by rolling over"
        );
        let s1 = wb.worksheet_range("Data 1").unwrap();
        assert_eq!(s1.get_size().0, 4, "header + 3 rows");
        assert_eq!(s1.get_value((0, 0)), Some(&Data::String("ID".into())));
        assert!(
            matches!(s1.get_value((1, 0)), Some(Data::Float(f)) if *f == 1.0 )
                || matches!(s1.get_value((1, 0)), Some(Data::Int(1)))
        );
        assert_eq!(
            s1.get_value((1, 1)),
            Some(&Data::String("=1+1".into())),
            "text that looks like a formula stays text"
        );
        assert_eq!(
            s1.get_value((2, 0)),
            Some(&Data::String("12345678901234567890".into())),
            "20 digits would be rounded as a number: kept as text"
        );
        assert!(
            matches!(s1.get_value((1, 2)), Some(Data::DateTime(_))),
            "{:?}",
            s1.get_value((1, 2))
        );
        let s2 = wb.worksheet_range("Data 2").unwrap();
        assert_eq!(
            s2.get_size().0,
            3,
            "header repeated on the second sheet, then 2 rows"
        );
    }

    #[test]
    fn cancel_leaves_no_files_and_streaming_reads_in_batches() {
        let d = tmp();
        let rows: Vec<Vec<Option<&str>>> = (0..100).map(|_| vec![Some("x")]).collect();
        let mut c = cur(vec![col("A", "VARCHAR2(1)")], rows);
        let cancel = AtomicBool::new(false);
        let mut seen = 0;
        let r = export(
            &mut c,
            &d.join("out"),
            &ExportOptions {
                batch_size: 10,
                ..no_bom()
            },
            &cancel,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(r.rows, 100);
        assert_eq!(
            c.calls, 11,
            "10 batches of 10 plus the empty one: rows are never read all at once"
        );
        let mut c = cur(
            vec![col("A", "VARCHAR2(1)")],
            (0..100).map(|_| vec![Some("x")]).collect(),
        );
        cancel.store(true, Ordering::Relaxed);
        let r = export(
            &mut c,
            &d.join("cancelled"),
            &no_bom(),
            &cancel,
            &mut |_| seen += 1,
        )
        .unwrap();
        assert!(r.cancelled && r.files.is_empty());
        assert!(!d.join("cancelled.csv").exists() && !d.join("cancelled.csv.part").exists());
    }

    #[test]
    fn a_failing_cursor_removes_partial_output() {
        struct Boom(Vec<Column>, usize);
        impl Cursor for Boom {
            fn columns(&self) -> &[Column] {
                &self.0
            }
            fn next_batch(&mut self, _m: usize) -> Result<Vec<Row>> {
                self.1 += 1;
                if self.1 == 1 {
                    Ok(vec![vec![Some("1".into())]; 5])
                } else {
                    Err(Error::Database(
                        "ORA-03113: end-of-file on communication channel".into(),
                    ))
                }
            }
        }
        let d = tmp();
        let e = export(
            &mut Boom(vec![col("A", "NUMBER")], 0),
            &d.join("out"),
            &no_bom(),
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(e.to_string().contains("ORA-03113"));
        assert_eq!(
            fs::read_dir(&d).unwrap().count(),
            0,
            "nothing left behind, not even a .part file"
        );
    }

    #[test]
    fn recommendation_follows_size_and_purpose() {
        let r = recommend(Some(5_000), Purpose::Spreadsheet);
        assert_eq!((r.format, r.gzip), (Format::Xlsx, false));
        let r = recommend(Some(400_000), Purpose::Spreadsheet);
        assert_eq!(r.format, Format::Csv);
        assert_eq!(
            r.alternatives
                .iter()
                .find(|a| a.format == Format::Xlsx)
                .unwrap()
                .verdict,
            Verdict::Caution
        );
        let r = recommend(Some(3_000_000), Purpose::Spreadsheet);
        assert_eq!(
            (r.format, r.gzip, r.split_rows),
            (Format::Csv, true, Some(1_000_000))
        );
        assert_eq!(
            r.alternatives
                .iter()
                .find(|a| a.format == Format::Xlsx)
                .unwrap()
                .verdict,
            Verdict::Avoid
        );
        assert!(r.reasons.iter().any(|x| x.contains("1,048,576")));
        assert_eq!(
            recommend(Some(2_000), Purpose::LoadOracle).format,
            Format::SqlInsert
        );
        let r = recommend(Some(50_000_000), Purpose::LoadOracle);
        assert_eq!((r.format, r.gzip), (Format::Csv, true));
        assert!(r.tip.as_deref().unwrap().contains("Data Pump"));
        assert_eq!(
            r.alternatives
                .iter()
                .find(|a| a.format == Format::SqlInsert)
                .unwrap()
                .verdict,
            Verdict::Avoid
        );
        let r = recommend(None, Purpose::Spreadsheet);
        assert_eq!(r.format, Format::Csv, "unknown size: the streaming format");
        assert!(r.reasons.iter().any(|x| x.contains("unknown")));
        assert!(recommend(Some(10), Purpose::Archive).gzip);
    }

    #[test]
    fn sql_literals_and_json_numbers() {
        assert_eq!(sql_string_literal("it's"), "'it''s'");
        assert_eq!(json_number(".5").as_deref(), Some("0.5"));
        assert_eq!(json_number("-.5").as_deref(), Some("-0.5"));
        assert_eq!(json_number("1E+25").as_deref(), Some("1E+25"));
        assert_eq!(json_number("1234567890123456"), None);
        assert_eq!(json_number("abc"), None);
        assert_eq!(json_number("5."), None);
        assert_eq!(
            ColKind::from_type("TIMESTAMP(6) WITH LOCAL TIME ZONE"),
            ColKind::TimestampTz
        );
        assert_eq!(ColKind::from_type("LONG RAW"), ColKind::Binary);
        assert_eq!(ColKind::from_type("CLOB"), ColKind::Text);
    }

    /// Produces rows on demand, so the benchmark never holds the data set in memory.
    struct Gen {
        cols: Vec<Column>,
        next: u64,
        total: u64,
    }
    impl Cursor for Gen {
        fn columns(&self) -> &[Column] {
            &self.cols
        }
        fn next_batch(&mut self, max: usize) -> Result<Vec<Row>> {
            let end = (self.next + max as u64).min(self.total);
            let rows = (self.next..end)
                .map(|i| {
                    vec![
                        Some(i.to_string()),
                        Some(format!("Customer number {i} with a moderately long name")),
                        Some(format!("{}.{:02}", i % 9000, i % 100)),
                        Some(format!("2026-01-{:02} 10:{:02}:00", i % 28 + 1, i % 60)),
                        Some("ACTIVE".into()),
                    ]
                })
                .collect();
            self.next = end;
            Ok(rows)
        }
    }

    /// `cargo test -p ora-core --release export_throughput -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn export_throughput() {
        let cols = vec![
            col("ID", "NUMBER(12)"),
            col("NAME", "VARCHAR2(60)"),
            col("AMOUNT", "NUMBER(10,2)"),
            col("CREATED", "DATE"),
            col("STATUS", "VARCHAR2(10)"),
        ];
        let d = tmp();
        for (label, fmt, gz, rows) in [
            ("csv", Format::Csv, false, 1_000_000u64),
            ("csv.gz", Format::Csv, true, 1_000_000),
            ("jsonl", Format::JsonLines, false, 1_000_000),
            ("sql insert", Format::SqlInsert, false, 200_000),
            ("xlsx", Format::Xlsx, false, 200_000),
        ] {
            let mut g = Gen {
                cols: cols.clone(),
                next: 0,
                total: rows,
            };
            let r = export(
                &mut g,
                &d.join("bench"),
                &ExportOptions {
                    format: fmt,
                    gzip: gz,
                    insert_table: Some("\"T\"".into()),
                    ..Default::default()
                },
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
            println!(
                "{label:>11}: {:>9} rows in {:>6} ms = {:>8} rows/s, {:>6.1} MB",
                r.rows,
                r.elapsed_ms,
                r.rows_per_sec,
                r.bytes as f64 / 1_048_576.0
            );
        }
    }
}
