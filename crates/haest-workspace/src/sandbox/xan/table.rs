//! A CSV in and out: the input (a file or the pipe), its delimiter, the table
//! it holds, and writing rows back out.

use std::io;

use haest_core::{fs::resolve_path, shell_command::ShellCall};

use super::{Clock, Fail, R, cli::Parsed, fail, usage};

pub(super) struct Table {
    pub(super) headers: Vec<String>,
    pub(super) rows: Vec<Vec<String>>,
    /// False under -n: then no verb writes a header row of its own.
    pub(super) has_headers: bool,
}

pub(super) fn cell(row: &[String], i: usize) -> &str {
    row.get(i).map_or("", String::as_str)
}

pub(super) async fn input(call: &ShellCall, file: Option<&str>) -> R<String> {
    match file {
        Some(f) if f != "-" => {
            let bytes = call
                .fs
                .read(&resolve_path(&call.cwd, f))
                .await
                .map_err(|e| {
                    fail(match e.kind() {
                        io::ErrorKind::NotFound => format!("{f}: no such file"),
                        _ => format!("{f}: {e}"),
                    })
                })?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => call
            .stdin_text()
            .map(|s| s.into_owned())
            .ok_or_else(|| usage(None, "no input: give a CSV file, or pipe one in")),
    }
}

pub(super) fn delimiter(p: &Parsed, file: Option<&str>, text: &str) -> R<u8> {
    if let Some(d) = p.get("delimiter") {
        return match d {
            "\\t" | "\t" | "tab" => Ok(b'\t'),
            d if d.len() == 1 => Ok(d.as_bytes()[0]),
            _ => Err(usage(None, "-d takes one character (\\t for a tab)")),
        };
    }
    if file.is_some_and(|f| f.ends_with(".tsv") || f.ends_with(".tab")) {
        return Ok(b'\t');
    }
    // A header line with tabs (or semicolons) and no comma is not comma separated.
    let first = text.lines().next().unwrap_or("");
    Ok(match () {
        _ if first.contains(',') => b',',
        _ if first.contains('\t') => b'\t',
        _ if first.contains(';') => b';',
        _ => b',',
    })
}

pub(super) fn read_table(text: &str, delim: u8, has_headers: bool, clock: &mut Clock) -> R<Table> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records() {
        clock.tick()?;
        let record = record.map_err(|e| fail(format!("the CSV does not parse: {e}")))?;
        rows.push(record.iter().map(str::to_string).collect::<Vec<_>>());
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let headers = if has_headers && !rows.is_empty() {
        let mut h = rows.remove(0);
        h.resize(width, String::new());
        h
    } else {
        (0..width).map(|i| i.to_string()).collect()
    };
    Ok(Table {
        headers,
        rows,
        has_headers,
    })
}

pub(super) fn write_csv(
    header: Option<&[String]>,
    rows: impl IntoIterator<Item = Vec<String>>,
    clock: &mut Clock,
) -> R<String> {
    let mut w = csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new());
    fn broke(e: impl std::fmt::Display) -> Fail {
        fail(format!("writing the CSV: {e}"))
    }
    if let Some(h) = header {
        w.write_record(h).map_err(broke)?;
    }
    for r in rows {
        clock.tick()?;
        w.write_record(&r).map_err(broke)?;
    }
    let bytes = w.into_inner().map_err(broke)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
