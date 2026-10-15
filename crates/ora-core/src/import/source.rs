//! Reading import files: CSV/TSV (delimiter, header and encoding detected, optionally
//! gzipped) and Excel. Everything is yielded as strings; typing happens later.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use encoding_rs::{Encoding, WINDOWS_1252};
use encoding_rs_io::DecodeReaderBytesBuilder;
use flate2::read::MultiGzDecoder;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadOptions {
    /// `None`: detect.
    pub delimiter: Option<char>,
    pub has_header: Option<bool>,
    /// Label such as `utf-8` or `windows-1252`; `None`: detect.
    pub encoding: Option<String>,
    /// Excel sheet name; `None`: the first sheet.
    pub sheet: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub format: String,
    pub delimiter: Option<char>,
    pub has_header: bool,
    pub encoding: String,
    pub sheets: Vec<String>,
}

pub trait RowSource: Send {
    fn headers(&self) -> &[String];
    fn info(&self) -> &SourceInfo;
    /// Next data row (the header is never returned). `None` at the end.
    fn next_row(&mut self) -> Result<Option<Vec<String>>>;
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("cannot read file: {e}"))
}

fn is_gz(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gz"))
}

fn inner_ext(path: &Path) -> String {
    let p = if is_gz(path) {
        path.with_extension("")
    } else {
        path.to_path_buf()
    };
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn raw_reader(path: &Path) -> Result<Box<dyn Read + Send>> {
    let f = File::open(path).map_err(err)?;
    Ok(if is_gz(path) {
        Box::new(MultiGzDecoder::new(f))
    } else {
        Box::new(f)
    })
}

/// UTF-8 if the sample is valid UTF-8 (a character cut off at the end is fine), else Windows-1252.
fn detect_encoding(sample: &[u8]) -> &'static Encoding {
    if sample.starts_with(&[0xFF, 0xFE]) || sample.starts_with(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16LE; // BOM sniffing in the decoder picks the real one
    }
    match std::str::from_utf8(sample) {
        Ok(_) => encoding_rs::UTF_8,
        Err(e) if e.error_len().is_none() => encoding_rs::UTF_8,
        Err(_) => WINDOWS_1252,
    }
}

fn read_sample(path: &Path, n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    let mut r = raw_reader(path)?;
    let mut got = 0;
    while got < n {
        let k = r.read(&mut buf[got..]).map_err(err)?;
        if k == 0 {
            break;
        }
        got += k;
    }
    buf.truncate(got);
    Ok(buf)
}

fn parse_records(text: &str, delim: u8, max: usize) -> Vec<Vec<String>> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut out = Vec::new();
    for rec in rdr.records().take(max) {
        match rec {
            Ok(r) => out.push(r.iter().map(String::from).collect()),
            Err(_) => break,
        }
    }
    out
}

/// Pick the delimiter that gives the most consistent multi-column rows.
fn sniff_delimiter(text: &str) -> char {
    let mut best = (',', -1.0f64);
    for d in [',', ';', '\t', '|'] {
        let recs = parse_records(text, d as u8, 60);
        if recs.is_empty() {
            continue;
        }
        let mut counts = std::collections::HashMap::new();
        for r in &recs {
            *counts.entry(r.len()).or_insert(0usize) += 1;
        }
        let (&cols, &freq) = counts.iter().max_by_key(|(c, f)| (**f, **c)).unwrap();
        let score = if cols > 1 {
            freq as f64 / recs.len() as f64 + cols as f64 * 0.001
        } else {
            0.0
        };
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

/// A first row of unique, non-empty, non-numeric, non-date names above rows that look like data.
fn looks_like_header(first: &[String], second: Option<&Vec<String>>) -> bool {
    let datalike = |s: &str| {
        let t = s.trim();
        t.parse::<f64>().is_ok()
            || (t.chars().any(|c| c.is_ascii_digit())
                && t.chars().any(|c| matches!(c, '-' | '/' | '.' | ':'))
                && t.chars().filter(|c| c.is_ascii_digit()).count() >= 4)
    };
    let filled = first.iter().filter(|c| !c.trim().is_empty()).count();
    // Names may repeat or be blank (fixed up later); what matters is that nothing in the row
    // looks like data. A first row of plain words is assumed to be a header.
    let _ = second;
    !first.is_empty() && filled * 2 >= first.len() && !first.iter().any(|c| datalike(c))
}

fn clean_headers(raw: Vec<String>) -> Vec<String> {
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    raw.into_iter()
        .enumerate()
        .map(|(i, h)| {
            let h = h.trim().trim_start_matches('\u{feff}').to_string();
            let h = if h.is_empty() {
                format!("COLUMN_{}", i + 1)
            } else {
                h
            };
            let n = seen.entry(h.to_lowercase()).or_insert(0);
            *n += 1;
            if *n > 1 {
                format!("{h}_{n}")
            } else {
                h
            }
        })
        .collect()
}

pub struct CsvSource {
    rdr: csv::Reader<Box<dyn Read + Send>>,
    headers: Vec<String>,
    pending: Option<Vec<String>>,
    info: SourceInfo,
}

impl CsvSource {
    pub fn open(path: &Path, opts: &ReadOptions) -> Result<Self> {
        let sample = read_sample(path, 256 * 1024)?;
        let enc = match &opts.encoding {
            Some(l) => Encoding::for_label(l.as_bytes())
                .ok_or_else(|| Error::Invalid(format!("unknown encoding {l:?}")))?,
            None => detect_encoding(&sample),
        };
        let decoded_sample = {
            let mut d = DecodeReaderBytesBuilder::new()
                .encoding(Some(enc))
                .build(&sample[..]);
            let mut s = String::new();
            // a multi-byte character may be cut at the end of the sample: ignore that tail error
            let _ = d.read_to_string(&mut s);
            s
        };
        let delim = opts
            .delimiter
            .unwrap_or_else(|| sniff_delimiter(&decoded_sample));
        let head = parse_records(&decoded_sample, delim as u8, 3);
        let has_header = opts.has_header.unwrap_or_else(|| {
            head.first()
                .is_some_and(|f| looks_like_header(f, head.get(1)))
        });

        let decoder = DecodeReaderBytesBuilder::new()
            .encoding(Some(enc))
            .build(raw_reader(path)?);
        let boxed: Box<dyn Read + Send> = Box::new(decoder);
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(delim as u8)
            .has_headers(false)
            .flexible(true)
            .from_reader(boxed);
        let mut first = csv::StringRecord::new();
        let got = rdr.read_record(&mut first).map_err(err)?;
        let first: Vec<String> = if got {
            first.iter().map(String::from).collect()
        } else {
            vec![]
        };
        let (headers, pending) = if has_header {
            (clean_headers(first), None)
        } else {
            let n = first.len();
            (
                (1..=n).map(|i| format!("COLUMN_{i}")).collect(),
                if n > 0 { Some(first) } else { None },
            )
        };
        let info = SourceInfo {
            format: if delim == '\t' {
                "tsv".into()
            } else {
                "csv".into()
            },
            delimiter: Some(delim),
            has_header,
            encoding: enc.name().to_string(),
            sheets: vec![],
        };
        Ok(Self {
            rdr,
            headers,
            pending,
            info,
        })
    }
}

impl RowSource for CsvSource {
    fn headers(&self) -> &[String] {
        &self.headers
    }
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn next_row(&mut self) -> Result<Option<Vec<String>>> {
        if let Some(p) = self.pending.take() {
            return Ok(Some(p));
        }
        let mut rec = csv::StringRecord::new();
        loop {
            match self.rdr.read_record(&mut rec) {
                Ok(false) => return Ok(None),
                Ok(true) => {
                    // Rows of only separators (`,,,` spacer rows left by spreadsheets) carry no data.
                    // Truly empty lines were already skipped by the csv reader, and a single empty
                    // value in a one-column file is a real NULL, so it is kept.
                    if rec.len() > 1 && rec.iter().all(|c| c.trim().is_empty()) {
                        continue;
                    }
                    return Ok(Some(rec.iter().map(String::from).collect()));
                }
                Err(e) => return Err(err(e)),
            }
        }
    }
}

pub struct XlsxSource {
    rows: std::vec::IntoIter<Vec<String>>,
    headers: Vec<String>,
    info: SourceInfo,
}

fn cell_text(d: &calamine::Data) -> String {
    use calamine::Data;
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::Float(f) => f.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(n) if n.time() == chrono::NaiveTime::MIN => n.format("%Y-%m-%d").to_string(),
            Some(n) => n.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => dt.as_f64().to_string(),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#{e:?}"),
    }
}

impl XlsxSource {
    pub fn open(path: &Path, opts: &ReadOptions) -> Result<Self> {
        use calamine::{open_workbook_auto, Reader};
        let mut wb = open_workbook_auto(path).map_err(err)?;
        let sheets = wb.sheet_names();
        let name = match &opts.sheet {
            Some(s) if sheets.contains(s) => s.clone(),
            Some(s) => return Err(Error::Invalid(format!("no sheet named {s:?}"))),
            None => sheets
                .first()
                .cloned()
                .ok_or_else(|| Error::Invalid("the workbook has no sheets".into()))?,
        };
        let range = wb.worksheet_range(&name).map_err(err)?;
        let mut rows: Vec<Vec<String>> = range
            .rows()
            .map(|r| r.iter().map(cell_text).collect())
            .collect();
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        for r in &mut rows {
            r.resize(width, String::new());
        }
        let has_header = opts.has_header.unwrap_or_else(|| {
            rows.first()
                .is_some_and(|f| looks_like_header(f, rows.get(1)))
        });
        let headers = if has_header && !rows.is_empty() {
            clean_headers(rows.remove(0))
        } else {
            (1..=width).map(|i| format!("COLUMN_{i}")).collect()
        };
        let info = SourceInfo {
            format: "xlsx".into(),
            delimiter: None,
            has_header,
            encoding: "utf-8".into(),
            sheets,
        };
        Ok(Self {
            rows: rows.into_iter(),
            headers,
            info,
        })
    }
}

impl RowSource for XlsxSource {
    fn headers(&self) -> &[String] {
        &self.headers
    }
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn next_row(&mut self) -> Result<Option<Vec<String>>> {
        loop {
            match self.rows.next() {
                None => return Ok(None),
                Some(r) if r.iter().all(|c| c.trim().is_empty()) => continue,
                Some(r) => return Ok(Some(r)),
            }
        }
    }
}

/// Open by extension: `.xlsx/.xlsm/.xls` -> Excel, anything else -> delimited text (`.gz` allowed).
pub fn open_source(path: &Path, opts: &ReadOptions) -> Result<Box<dyn RowSource>> {
    match inner_ext(path).as_str() {
        "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" => Ok(Box::new(XlsxSource::open(path, opts)?)),
        _ => Ok(Box::new(CsvSource::open(path, opts)?)),
    }
}

/// Size on disk, for progress percentages.
pub fn file_size(path: &Path) -> u64 {
    File::open(path)
        .and_then(|mut f| f.seek(SeekFrom::End(0)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);
    pub(crate) fn tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ora-ide-imp-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }
    fn all(mut s: Box<dyn RowSource>) -> (Vec<String>, Vec<Vec<String>>, SourceInfo) {
        let mut rows = vec![];
        while let Some(r) = s.next_row().unwrap() {
            rows.push(r);
        }
        (s.headers().to_vec(), rows, s.info().clone())
    }

    #[test]
    fn detects_delimiter_header_and_handles_quotes_and_newlines() {
        for (d, name) in [
            (",", "a.csv"),
            (";", "b.csv"),
            ("\t", "c.tsv"),
            ("|", "d.txt"),
        ] {
            let body = format!(
                "id{d}name{d}note\n1{d}\"Ann, the \"\"boss\"\"\"{d}\"two\nlines\"\n2{d}Bob{d}\n"
            );
            let (h, rows, info) =
                all(open_source(&tmp(name, body.as_bytes()), &ReadOptions::default()).unwrap());
            assert_eq!(h, vec!["id", "name", "note"], "{name}");
            assert_eq!(info.delimiter, Some(d.chars().next().unwrap()), "{name}");
            assert!(info.has_header);
            assert_eq!(
                rows[0],
                vec!["1", "Ann, the \"boss\"", "two\nlines"],
                "{name}"
            );
            assert_eq!(rows.len(), 2, "{name}");
        }
    }

    #[test]
    fn headerless_files_get_generated_names_and_keep_their_first_row() {
        let (h, rows, info) = all(open_source(
            &tmp("x.csv", b"1,2020-01-05,3.5\n2,2020-01-06,4.5\n"),
            &ReadOptions::default(),
        )
        .unwrap());
        assert_eq!(h, vec!["COLUMN_1", "COLUMN_2", "COLUMN_3"]);
        assert!(!info.has_header);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        // an explicit choice overrides detection
        let (h, rows, _) = all(open_source(
            &tmp("y.csv", b"1,2\n3,4\n"),
            &ReadOptions {
                has_header: Some(true),
                ..Default::default()
            },
        )
        .unwrap());
        assert_eq!((h, rows.len()), (vec!["1".to_string(), "2".to_string()], 1));
    }

    #[test]
    fn duplicate_and_empty_headers_are_made_unique() {
        let (h, _, _) = all(open_source(
            &tmp("x.csv", b"name,Name,,name\na,b,c,d\n"),
            &ReadOptions::default(),
        )
        .unwrap());
        assert_eq!(h, vec!["name", "Name_2", "COLUMN_3", "name_3"]);
    }

    #[test]
    fn encodings_bom_utf8_latin1_and_utf16() {
        // UTF-8 with BOM (what Excel writes)
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("id,name\n1,Zoë\n".as_bytes());
        let (h, rows, info) =
            all(open_source(&tmp("a.csv", &bom), &ReadOptions::default()).unwrap());
        assert_eq!(
            (h[0].as_str(), rows[0][1].as_str(), info.encoding.as_str()),
            ("id", "Zoë", "UTF-8"),
            "BOM must not end up in the first header"
        );
        // Windows-1252 (not valid UTF-8): é is 0xE9
        let latin = [b"id,name\n1,Jos".as_slice(), &[0xE9], b"\n"].concat();
        let (_, rows, info) =
            all(open_source(&tmp("b.csv", &latin), &ReadOptions::default()).unwrap());
        assert_eq!(
            (rows[0][1].as_str(), info.encoding.as_str()),
            ("José", "windows-1252")
        );
        // UTF-16 LE with BOM
        let mut u16b = vec![0xFF, 0xFE];
        for c in "id,name\n1,Ana\n".encode_utf16() {
            u16b.extend_from_slice(&c.to_le_bytes());
        }
        let (h, rows, _) = all(open_source(&tmp("c.csv", &u16b), &ReadOptions::default()).unwrap());
        assert_eq!((h[1].as_str(), rows[0][1].as_str()), ("name", "Ana"));
        // explicit override
        assert!(open_source(
            &tmp("d.csv", b"a\n1\n"),
            &ReadOptions {
                encoding: Some("nonsense".into()),
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn gzip_files_and_blank_lines() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let mut z = GzEncoder::new(Vec::new(), Compression::fast());
        z.write_all(b"a,b\n1,2\n\n3,4\n,\n,\n\n").unwrap();
        let (_, rows, _) = all(open_source(
            &tmp("t.csv.gz", &z.finish().unwrap()),
            &ReadOptions::default(),
        )
        .unwrap());
        assert_eq!(
            rows,
            vec![vec!["1", "2"], vec!["3", "4"]],
            "blank lines and ',' spacer rows are skipped"
        );
    }

    #[test]
    fn ragged_rows_are_passed_through_for_validation_to_report() {
        let (_, rows, _) = all(open_source(
            &tmp("r.csv", b"a,b,c\n1,2,3\n4,5\n6,7,8,9\n"),
            &ReadOptions::default(),
        )
        .unwrap());
        assert_eq!(rows.iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 2, 4]);
    }

    #[test]
    fn excel_round_trip_with_typed_cells() {
        use crate::db::{Column, Cursor, Row};
        use crate::export::{export, ExportOptions, Format};
        struct C(Vec<Column>, Option<Vec<Row>>);
        impl Cursor for C {
            fn columns(&self) -> &[Column] {
                &self.0
            }
            fn next_batch(&mut self, _m: usize) -> Result<Vec<Row>> {
                Ok(self.1.take().unwrap_or_default())
            }
        }
        let col = |n: &str, t: &str| Column {
            name: n.into(),
            data_type: t.into(),
            nullable: true,
        };
        let mut c = C(
            vec![
                col("ID", "NUMBER"),
                col("NAME", "VARCHAR2(9)"),
                col("D", "DATE"),
            ],
            Some(vec![
                vec![
                    Some("7".into()),
                    Some("Ann".into()),
                    Some("2020-02-29 00:00:00".into()),
                ],
                vec![Some("8".into()), None, Some("2021-05-06 13:14:15".into())],
            ]),
        );
        let dir = tmp("seed.txt", b"").parent().unwrap().to_path_buf();
        let rep = export(
            &mut c,
            &dir.join("book"),
            &ExportOptions {
                format: Format::Xlsx,
                ..Default::default()
            },
            &std::sync::atomic::AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let (h, rows, info) =
            all(open_source(Path::new(&rep.files[0]), &ReadOptions::default()).unwrap());
        assert_eq!(h, vec!["ID", "NAME", "D"]);
        assert_eq!(info.format, "xlsx");
        assert_eq!(
            rows[0],
            vec!["7", "Ann", "2020-02-29"],
            "date-only cell has no time part; numbers have no .0"
        );
        assert_eq!(rows[1], vec!["8", "", "2021-05-06 13:14:15"]);
    }
}
