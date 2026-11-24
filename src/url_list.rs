//! Bounded parsing, scope checks, and coverage rows for URL-list audits.
//!
//! A URL list is deliberately separate from link discovery: every submitted row is retained
//! with its source position, and only eligible unique URLs are handed to the crawl engine.
//! Parsing never touches the network and every buffer is bounded before it reaches the UI.

use std::io::Read;
use std::path::Path;

use url::Url;

use crate::crawl;

/// Total input size accepted from paste or a local file.
pub const MAX_INPUT_BYTES: u64 = 5 * 1024 * 1024;
/// Input records accepted from paste, TXT, or CSV.
pub const MAX_RECORDS: usize = 50_000;
/// The maximum bytes retained for one URL field.
pub const MAX_URL_BYTES: usize = 8 * 1024;
/// The CSV parser retains one header plus the data-row budget.
const MAX_CSV_RECORDS: usize = MAX_RECORDS + 1;

/// How a URL list arrived. Only the source label differs; parsing rules are otherwise shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    Paste,
    Txt,
    Csv,
}

impl InputFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Paste => "pasted list",
            Self::Txt => "TXT file",
            Self::Csv => "CSV file",
        }
    }
}

/// A hard input error. These reject the whole draft before any crawl can be scheduled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListError {
    TooLarge { bytes: u64, limit: u64 },
    TooManyRecords { found: usize, limit: usize },
    NotUtf8,
    Empty,
    MissingUrlHeader,
    MultipleUrlHeaders,
    MalformedCsv { record: u64, detail: String },
    Io(String),
}

impl ListError {
    pub fn message(&self) -> String {
        match self {
            Self::TooLarge { bytes, limit } => format!(
                "URL list is {} ({bytes} bytes); the limit is {} ({limit} bytes).",
                bytes_label(*bytes),
                bytes_label(*limit)
            ),
            Self::TooManyRecords { found, limit } => {
                format!("URL list has {found} records; the limit is {limit}.")
            }
            Self::NotUtf8 => "URL list is not valid UTF-8 text.".into(),
            Self::Empty => "The URL list is empty.".into(),
            Self::MissingUrlHeader => "CSV needs a header row with one column named URL.".into(),
            Self::MultipleUrlHeaders => {
                "CSV has more than one column named URL; keep exactly one.".into()
            }
            Self::MalformedCsv { record, detail } => {
                format!("CSV record {record} is malformed: {detail}.")
            }
            Self::Io(detail) => format!("Could not read the URL list: {detail}"),
        }
    }
}

fn bytes_label(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// The site scope named by the seed field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    host: String,
    /// A non-default port explicitly named by the seed. Default ports stay available per scheme.
    explicit_port: Option<u16>,
}

impl Scope {
    pub fn from_seed(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("Enter a site URL to define the list scope.".into());
        }
        let with_scheme = if trimmed.contains("://") {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };
        let url = Url::parse(&with_scheme)
            .map_err(|_| format!("Couldn't read “{trimmed}” as a site URL."))?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err("Use an http or https site URL.".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("The site scope cannot embed credentials.".into());
        }
        let host = url
            .host_str()
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
            .ok_or_else(|| "That site URL has no host.".to_string())?;
        Ok(Self {
            host,
            explicit_port: url.port(),
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// The normalized seed hostname and any port a local or staging fixture explicitly names.
    pub fn allows(&self, url: &Url) -> bool {
        if url.scheme() != "http" && url.scheme() != "https" {
            return false;
        }
        if !url
            .host_str()
            .is_some_and(|host| host.trim_end_matches('.').eq_ignore_ascii_case(&self.host))
        {
            return false;
        }
        let Some(port) = url.port() else {
            // No explicit port means the scheme's default port.
            return true;
        };
        if Some(port) == self.explicit_port {
            return true;
        }
        // An explicit default port is normalized away by `url`, so a remaining port must match
        // the seed's explicit port. This keeps ordinary scopes on their default origins.
        false
    }

    fn scope_reason(&self, url: &Url) -> String {
        if url.scheme() != "http" && url.scheme() != "https" {
            return format!("Unsupported scheme ({})", url.scheme());
        }
        if !url
            .host_str()
            .is_some_and(|host| host.trim_end_matches('.').eq_ignore_ascii_case(&self.host))
        {
            return format!(
                "Host {} is outside the site scope {}",
                url.host_str().unwrap_or("unknown"),
                self.host
            );
        }
        "Port is outside the site scope".into()
    }
}

/// One parsed row and its current coverage result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub source_row: u64,
    pub original: String,
    pub normalized: String,
    pub outcome: Outcome,
    pub reason: String,
    pub status: u16,
    pub final_url: String,
    pub duplicate_of: Option<u64>,
}

/// One engine-produced result for a submitted list row. Preview-only rows (duplicate, invalid and
/// out of scope) keep their outcome in [`Entry`] and are not checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListCheck {
    pub source_row: u64,
    pub outcome: Outcome,
    pub reason: String,
    pub status: u16,
    pub final_url: String,
}

/// The typed per-input state shown in coverage and exported to CSV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Unique and in scope; the engine has not produced a result yet.
    Eligible,
    Duplicate,
    Invalid,
    OutOfScope,
    /// An HTTP response, including 4xx and 5xx.
    Checked,
    /// A fetch or transport failure.
    Failed,
    /// Robots or navigation policy refused the request.
    Blocked,
    /// A page limit, cancellation, or startup prevented a check.
    Unchecked,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Eligible => "Eligible",
            Self::Duplicate => "Duplicate",
            Self::Invalid => "Invalid",
            Self::OutOfScope => "Out of scope",
            Self::Checked => "Checked",
            Self::Failed => "Failed",
            Self::Blocked => "Blocked",
            Self::Unchecked => "Unchecked",
        }
    }

    /// Rows worth opening first in the existing All/Findings/Incomplete filter.
    pub fn is_finding(self) -> bool {
        matches!(self, Self::Failed | Self::Blocked)
    }

    pub fn is_incomplete(self) -> bool {
        matches!(self, Self::Unchecked)
    }
}

/// An eligible unique URL, ready to become an explicit crawl seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    pub source_row: u64,
    pub original: String,
    pub url: String,
}

/// The full bounded preview. All input rows remain available for coverage and export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub format: InputFormat,
    pub entries: Vec<Entry>,
    pub eligible: usize,
    pub duplicate: usize,
    pub invalid: usize,
    pub out_of_scope: usize,
}

impl Preview {
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn has_eligible(&self) -> bool {
        self.eligible > 0
    }

    pub fn seeds(&self) -> Vec<Seed> {
        self.entries
            .iter()
            .filter(|entry| entry.outcome == Outcome::Eligible)
            .map(|entry| Seed {
                source_row: entry.source_row,
                original: entry.original.clone(),
                url: entry.normalized.clone(),
            })
            .collect()
    }

    /// Build the initial coverage rows. Eligible rows become Unchecked until the engine reports
    /// a typed result; the other rows already have a final local outcome.
    pub fn coverage_rows(&self) -> Vec<Entry> {
        self.entries
            .iter()
            .cloned()
            .map(|mut entry| {
                if entry.outcome == Outcome::Eligible {
                    entry.outcome = Outcome::Unchecked;
                    entry.reason = "Queued for checking".into();
                }
                entry
            })
            .collect()
    }
}

/// Read a TXT or CSV file with a hard byte ceiling. The caller runs this away from the UI thread.
pub fn read_file(path: &Path) -> Result<Vec<u8>, ListError> {
    let file = std::fs::File::open(path).map_err(|error| ListError::Io(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ListError::Io(error.to_string()))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ListError::TooLarge {
            bytes: bytes.len() as u64,
            limit: MAX_INPUT_BYTES,
        });
    }
    Ok(bytes)
}

/// Parse pasted text as newline-separated URLs.
pub fn parse_paste(text: &str, scope: &Scope) -> Result<Preview, ListError> {
    check_size(text.len() as u64)?;
    parse_lines(text, InputFormat::Paste, scope)
}

/// Parse a local UTF-8 TXT file. A leading BOM and CRLF line endings are accepted.
pub fn parse_txt(bytes: &[u8], scope: &Scope) -> Result<Preview, ListError> {
    check_size(bytes.len() as u64)?;
    let text = std::str::from_utf8(bytes).map_err(|_| ListError::NotUtf8)?;
    parse_lines(strip_bom(text), InputFormat::Txt, scope)
}

/// Parse a local UTF-8 CSV file with exactly one case-insensitive `URL` header.
pub fn parse_csv(bytes: &[u8], scope: &Scope) -> Result<Preview, ListError> {
    check_size(bytes.len() as u64)?;
    let text = std::str::from_utf8(bytes).map_err(|_| ListError::NotUtf8)?;
    let text = strip_bom(text);
    let records = parse_csv_records(text)?;
    let Some(header) = records.first() else {
        return Err(ListError::Empty);
    };
    let mut url_columns = header
        .fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.trim().eq_ignore_ascii_case("url"));
    let Some((url_index, _)) = url_columns.next() else {
        return Err(ListError::MissingUrlHeader);
    };
    if url_columns.next().is_some() {
        return Err(ListError::MultipleUrlHeaders);
    }

    let mut records_seen = 0usize;
    let mut rows = Vec::new();
    for record in records.iter().skip(1) {
        if csv_record_is_blank(&record.fields) {
            continue;
        }
        records_seen += 1;
        if records_seen > MAX_RECORDS {
            return Err(ListError::TooManyRecords {
                found: records_seen,
                limit: MAX_RECORDS,
            });
        }
        let original = record.fields.get(url_index).cloned().unwrap_or_default();
        rows.push((record.number, original));
    }
    if rows.is_empty() {
        return Err(ListError::Empty);
    }
    Ok(classify(rows, InputFormat::Csv, scope))
}

fn check_size(bytes: u64) -> Result<(), ListError> {
    if bytes > MAX_INPUT_BYTES {
        Err(ListError::TooLarge {
            bytes,
            limit: MAX_INPUT_BYTES,
        })
    } else {
        Ok(())
    }
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn parse_lines(text: &str, format: InputFormat, scope: &Scope) -> Result<Preview, ListError> {
    let mut rows = Vec::new();
    let mut seen = 0usize;
    for (index, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        seen += 1;
        if seen > MAX_RECORDS {
            return Err(ListError::TooManyRecords {
                found: seen,
                limit: MAX_RECORDS,
            });
        }
        rows.push(((index + 1) as u64, line.to_string()));
    }
    if rows.is_empty() {
        return Err(ListError::Empty);
    }
    Ok(classify(rows, format, scope))
}

fn classify(rows: Vec<(u64, String)>, format: InputFormat, scope: &Scope) -> Preview {
    let mut entries = Vec::with_capacity(rows.len());
    // The first source row for each normalized URL owns the canonical entry.
    let mut first: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let (mut eligible, mut duplicate, mut invalid, mut out_of_scope) = (0, 0, 0, 0);

    for (source_row, original) in rows {
        let mut entry = Entry {
            source_row,
            original,
            normalized: String::new(),
            outcome: Outcome::Invalid,
            reason: String::new(),
            status: 0,
            final_url: String::new(),
            duplicate_of: None,
        };
        match validate(&entry.original, scope) {
            Ok(normalized) => {
                entry.normalized = normalized.clone();
                if let Some(first_row) = first.get(&normalized) {
                    duplicate += 1;
                    entry.outcome = Outcome::Duplicate;
                    entry.reason = format!("Duplicate of row {first_row}");
                    entry.duplicate_of = Some(*first_row);
                } else {
                    eligible += 1;
                    entry.outcome = Outcome::Eligible;
                    entry.reason = "Unique and in scope".into();
                    first.insert(normalized, source_row);
                }
            }
            Err(Rejected::Invalid(reason)) => {
                invalid += 1;
                entry.reason = reason;
            }
            Err(Rejected::OutOfScope(reason)) => {
                out_of_scope += 1;
                entry.outcome = Outcome::OutOfScope;
                entry.reason = reason;
            }
        }
        entries.push(entry);
    }

    Preview {
        format,
        entries,
        eligible,
        duplicate,
        invalid,
        out_of_scope,
    }
}

enum Rejected {
    Invalid(String),
    OutOfScope(String),
}

fn validate(raw: &str, scope: &Scope) -> Result<String, Rejected> {
    if raw.len() > MAX_URL_BYTES {
        return Err(Rejected::Invalid(format!(
            "URL exceeds the 8 KiB field limit ({} bytes)",
            raw.len()
        )));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Rejected::Invalid("Empty URL".into()));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = Url::parse(&with_scheme)
        .map_err(|_| Rejected::Invalid(format!("Couldn't read “{trimmed}” as a URL")))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(Rejected::Invalid(format!(
            "Unsupported scheme ({})",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Rejected::Invalid("URL embeds credentials".into()));
    }
    if url.host_str().is_none() {
        return Err(Rejected::Invalid("URL has no host".into()));
    }
    if !scope.allows(&url) {
        return Err(Rejected::OutOfScope(scope.scope_reason(&url)));
    }
    Ok(crawl::url_key(url))
}

#[derive(Clone, Debug)]
struct CsvRecord {
    number: u64,
    fields: Vec<String>,
}

fn finish_csv_field(fields: &mut Vec<String>, field: &mut String, field_started: &mut bool) {
    fields.push(std::mem::take(field));
    *field_started = false;
}

/// A blank physical record is one empty field. Whitespace-only lines are treated the same way,
/// matching TXT/paste input, but a record with multiple empty columns remains an invalid row.
fn csv_record_is_blank(fields: &[String]) -> bool {
    fields.len() == 1 && fields[0].trim().is_empty()
}

/// Retain one record, or skip a blank line, while enforcing the data-record cap during parsing.
/// A file with millions of newline-only records must fail without allocating them first, and
/// blank lines never consume the 50,000-row budget.
fn push_csv_record(
    records: &mut Vec<CsvRecord>,
    number: &mut u64,
    fields: &mut Vec<String>,
    field: &mut String,
    field_started: &mut bool,
) -> Result<(), ListError> {
    finish_csv_field(fields, field, field_started);
    if csv_record_is_blank(fields) {
        fields.clear();
    } else {
        // Retain one header plus exactly `MAX_RECORDS` data rows. Reject the next nonblank
        // record before retaining it, so the allocation stays bounded even for a hostile file.
        if records.len() >= MAX_CSV_RECORDS {
            return Err(ListError::TooManyRecords {
                found: MAX_RECORDS + 1,
                limit: MAX_RECORDS,
            });
        }
        records.push(CsvRecord {
            number: *number,
            fields: std::mem::take(fields),
        });
    }
    *number += 1;
    Ok(())
}

/// A small RFC-4180-style parser: quoted fields, doubled quotes, embedded newlines, CRLF, and
/// one record counter for source rows. Malformed quoting rejects the whole file.
fn parse_csv_records(text: &str) -> Result<Vec<CsvRecord>, ListError> {
    let mut records = Vec::new();
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quote_closed = false;
    let mut field_started = false;
    // 1-based record number. Header is 1; data records start at 2.
    let mut number = 1u64;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                        quote_closed = true;
                    }
                }
                other => field.push(other),
            }
            continue;
        }
        if quote_closed {
            match ch {
                ',' => {
                    quote_closed = false;
                    finish_csv_field(&mut fields, &mut field, &mut field_started);
                }
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    quote_closed = false;
                    push_csv_record(
                        &mut records,
                        &mut number,
                        &mut fields,
                        &mut field,
                        &mut field_started,
                    )?;
                }
                '\n' => {
                    quote_closed = false;
                    push_csv_record(
                        &mut records,
                        &mut number,
                        &mut fields,
                        &mut field,
                        &mut field_started,
                    )?;
                }
                other => {
                    return Err(ListError::MalformedCsv {
                        record: number,
                        detail: format!("unexpected “{other}” after a closing quote"),
                    });
                }
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() && !field_started => {
                in_quotes = true;
                field_started = true;
            }
            '"' => {
                return Err(ListError::MalformedCsv {
                    record: number,
                    detail: "quote inside an unquoted field".into(),
                });
            }
            ',' => finish_csv_field(&mut fields, &mut field, &mut field_started),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                push_csv_record(
                    &mut records,
                    &mut number,
                    &mut fields,
                    &mut field,
                    &mut field_started,
                )?;
            }
            '\n' => {
                push_csv_record(
                    &mut records,
                    &mut number,
                    &mut fields,
                    &mut field,
                    &mut field_started,
                )?;
            }
            other => {
                field_started = true;
                field.push(other);
            }
        }
    }

    if in_quotes {
        return Err(ListError::MalformedCsv {
            record: number,
            detail: "unclosed quoted field".into(),
        });
    }
    if quote_closed || field_started || !field.is_empty() || !fields.is_empty() {
        push_csv_record(
            &mut records,
            &mut number,
            &mut fields,
            &mut field,
            &mut field_started,
        )?;
    }
    if records.is_empty() {
        return Err(ListError::Empty);
    }
    Ok(records)
}

/// Coverage export columns, in a stable order.
pub const CSV_HEADERS: &[&str] = &[
    "SOURCE ROW",
    "ORIGINAL VALUE",
    "NORMALIZED URL",
    "OUTCOME",
    "REASON",
    "HTTP STATUS",
    "FINAL URL",
    "DUPLICATE OF ROW",
];

pub fn csv_row(entry: &Entry) -> Vec<String> {
    vec![
        entry.source_row.to_string(),
        entry.original.clone(),
        entry.normalized.clone(),
        entry.outcome.label().into(),
        entry.reason.clone(),
        if entry.status == 0 {
            String::new()
        } else {
            entry.status.to_string()
        },
        entry.final_url.clone(),
        entry
            .duplicate_of
            .map(|row| row.to_string())
            .unwrap_or_default(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope::from_seed("https://example.com/").expect("scope")
    }

    fn labels(preview: &Preview) -> Vec<(&str, u64, Option<u64>)> {
        preview
            .entries
            .iter()
            .map(|entry| (entry.outcome.label(), entry.source_row, entry.duplicate_of))
            .collect()
    }

    #[test]
    fn paste_keeps_queries_fragments_and_first_row_provenance() {
        let preview = parse_paste(
            "https://example.com/a?b=1#x\n\
             https://example.com/a?b=1#y\n\
             https://example.com/a?b=2\n\
             https://example.com/a?b=1\n",
            &scope(),
        )
        .expect("preview");
        assert_eq!(preview.total(), 4);
        assert_eq!(preview.eligible, 2);
        assert_eq!(preview.duplicate, 2);
        assert_eq!(preview.entries[0].normalized, "https://example.com/a?b=1");
        assert_eq!(preview.entries[1].normalized, "https://example.com/a?b=1");
        assert_eq!(preview.entries[1].duplicate_of, Some(1));
        assert_eq!(preview.entries[2].normalized, "https://example.com/a?b=2");
        assert_eq!(preview.entries[3].duplicate_of, Some(1));
        assert_eq!(
            labels(&preview),
            vec![
                ("Eligible", 1, None),
                ("Duplicate", 2, Some(1)),
                ("Eligible", 3, None),
                ("Duplicate", 4, Some(1))
            ]
        );
        assert_eq!(preview.seeds().len(), 2);
        assert_eq!(preview.seeds()[1].source_row, 3);
    }

    #[test]
    fn scope_is_exact_host_default_schemes_and_rejects_credentials_and_schemes() {
        let preview = parse_paste(
            "https://example.com/a\n\
             http://example.com:80/b\n\
             https://example.com:443/c\n\
             https://example.com:8443/d\n\
             https://cdn.example.com/e\n\
             ftp://example.com/f\n\
             https://user:pass@example.com/g\n\
             http://other.test/h\n",
            &scope(),
        )
        .expect("preview");
        assert_eq!(preview.eligible, 3);
        assert_eq!(preview.out_of_scope, 3);
        assert_eq!(preview.invalid, 2);
        assert_eq!(preview.entries[0].normalized, "https://example.com/a");
        assert_eq!(preview.entries[1].normalized, "http://example.com/b");
        assert_eq!(preview.entries[2].normalized, "https://example.com/c");
        assert_eq!(preview.entries[5].outcome, Outcome::Invalid);
        assert!(preview.entries[5].reason.contains("Unsupported scheme"));
        assert_eq!(preview.entries[6].outcome, Outcome::Invalid);
        assert!(preview.entries[6].reason.contains("credentials"));
    }

    #[test]
    fn a_seed_with_an_explicit_fixture_port_accepts_that_port() {
        let scope = Scope::from_seed("http://127.0.0.1:8123/").expect("scope");
        let preview = parse_paste("http://127.0.0.1:8123/a\nhttp://127.0.0.1:8124/b\n", &scope)
            .expect("preview");
        assert_eq!(preview.eligible, 1);
        assert_eq!(preview.out_of_scope, 1);
    }

    #[test]
    fn txt_accepts_bom_crlf_and_reports_physical_lines() {
        let bytes = b"\xEF\xBB\xBFhttps://example.com/a\r\n\r\nhttps://example.com/b\r\n";
        let preview = parse_txt(bytes, &scope()).expect("preview");
        assert_eq!(preview.eligible, 2);
        assert_eq!(preview.entries[0].source_row, 1);
        assert_eq!(preview.entries[1].source_row, 3);
    }

    #[test]
    fn csv_accepts_bom_crlf_quotes_multiline_and_reordered_columns() {
        let bytes = b"\xEF\xBB\xBFName,URL,Notes\r\n\
            \"A \"\"quoted\"\"\r\n\
            page\",https://example.com/a?x=1,\"two\r\nlines\"\r\n\
            Other, https://example.com/b ,plain\r\n";
        let preview = parse_csv(bytes, &scope()).expect("preview");
        assert_eq!(preview.eligible, 2);
        assert_eq!(preview.entries[0].source_row, 2);
        assert_eq!(preview.entries[0].original, "https://example.com/a?x=1");
        assert_eq!(preview.entries[0].normalized, "https://example.com/a?x=1");
        assert_eq!(preview.entries[1].source_row, 3);
        assert_eq!(preview.entries[1].original, " https://example.com/b ");
    }

    #[test]
    fn csv_header_is_case_insensitive_and_must_be_unique() {
        assert_eq!(
            parse_csv(b"Page,uRl\nx,https://example.com/\n", &scope())
                .unwrap()
                .eligible,
            1
        );
        assert_eq!(
            parse_csv(b"Page,Address\nx,https://example.com/\n", &scope()),
            Err(ListError::MissingUrlHeader)
        );
        assert_eq!(
            parse_csv(
                b"URL,url\nhttps://example.com/,https://example.com/\n",
                &scope()
            ),
            Err(ListError::MultipleUrlHeaders)
        );
    }

    #[test]
    fn malformed_csv_is_reported_with_the_record_number() {
        let error = parse_csv(b"URL\n\"https://example.com/unclosed\n", &scope())
            .expect_err("unclosed quote");
        assert_eq!(
            error,
            ListError::MalformedCsv {
                record: 2,
                detail: "unclosed quoted field".into()
            }
        );
        let error =
            parse_csv(b"URL\n\"https://example.com/\"x\n", &scope()).expect_err("text after quote");
        assert!(matches!(error, ListError::MalformedCsv { record: 2, .. }));
        let error = parse_csv(b"URL\nhttps://example.com/a\"b\n", &scope())
            .expect_err("quote inside unquoted field");
        assert_eq!(
            error,
            ListError::MalformedCsv {
                record: 2,
                detail: "quote inside an unquoted field".into()
            }
        );
    }

    #[test]
    fn csv_record_cap_is_enforced_while_parsing() {
        let mut csv = String::from("URL\n");
        for index in 0..=MAX_RECORDS {
            csv.push_str(&format!("https://example.com/{index}\n"));
        }
        let error = parse_csv(csv.as_bytes(), &scope()).expect_err("record cap");
        assert!(matches!(
            error,
            ListError::TooManyRecords { found, limit }
                if found == MAX_RECORDS + 1 && limit == MAX_RECORDS
        ));

        let mut allowed = String::from("URL\n");
        for index in 0..MAX_RECORDS {
            allowed.push_str(&format!("https://example.com/{index}\n"));
        }
        let preview = parse_csv(allowed.as_bytes(), &scope()).expect("exact cap");
        assert_eq!(preview.total(), MAX_RECORDS);
    }

    #[test]
    fn csv_blank_lines_do_not_consume_the_record_budget_or_become_rows() {
        let mut csv = String::from("\r\nURL\r\n\r\n   \r\nhttps://example.com/a\r\n\r\n");
        for index in 0..MAX_RECORDS - 1 {
            csv.push_str(&format!("https://example.com/{index}\n"));
        }
        let preview = parse_csv(csv.as_bytes(), &scope()).expect("blank lines skipped");
        assert_eq!(preview.total(), MAX_RECORDS);
        assert_eq!(preview.eligible, MAX_RECORDS);
        assert_eq!(preview.duplicate, 0);
        assert_eq!(preview.entries[0].source_row, 5);
        assert_eq!(preview.entries[0].original, "https://example.com/a");
    }

    #[test]
    fn all_invalid_lists_still_preview_rows_and_offer_no_seeds() {
        let preview = parse_paste(
            "ftp://example.com/a\nhttps://cdn.example.com/b\nuser:pass@example.com\n",
            &scope(),
        )
        .expect("preview");
        assert_eq!(preview.eligible, 0);
        assert!(!preview.has_eligible());
        assert_eq!(preview.total(), 3);
        assert!(preview.seeds().is_empty());
    }

    #[test]
    fn coverage_csv_preserves_row_order_and_spreadsheet_escaping() {
        let mut first = parse_paste(
            "https://example.com/a\nhttps://example.com/a\nftp://example.com/\n",
            &scope(),
        )
        .expect("preview")
        .coverage_rows();
        first[0].outcome = Outcome::Blocked;
        first[0].reason = "=SUM(A1:A2) \u{2014} \"quoted\"".into();
        first[0].status = 403;
        first[0].final_url = "https://example.com/a?ok=1".into();
        let rows = first.iter().map(csv_row).collect::<Vec<_>>();
        let csv = crate::export::to_csv(CSV_HEADERS, &rows);
        let lines = csv.lines().collect::<Vec<_>>();
        assert_eq!(
            lines[1],
            "\"1\",\"https://example.com/a\",\"https://example.com/a\",\"Blocked\",\"'=SUM(A1:A2) — \"\"quoted\"\"\",\"403\",\"https://example.com/a?ok=1\",\"\""
        );
        assert_eq!(
            lines[2],
            "\"2\",\"https://example.com/a\",\"https://example.com/a\",\"Duplicate\",\"Duplicate of row 1\",\"\",\"\",\"1\""
        );
        assert_eq!(
            lines[3],
            "\"3\",\"ftp://example.com/\",\"\",\"Invalid\",\"Unsupported scheme (ftp)\",\"\",\"\",\"\""
        );
    }

    #[test]
    fn hard_limits_reject_explicitly() {
        let too_big = "a".repeat(MAX_INPUT_BYTES as usize + 1);
        assert!(matches!(
            parse_paste(&too_big, &scope()),
            Err(ListError::TooLarge { .. })
        ));

        let many = (0..=MAX_RECORDS)
            .map(|index| format!("https://example.com/{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            parse_paste(&many, &scope()),
            Err(ListError::TooManyRecords { found, limit })
                if found == MAX_RECORDS + 1 && limit == MAX_RECORDS
        ));

        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        let preview = parse_paste(&long, &scope()).expect("preview");
        assert_eq!(preview.invalid, 1);
        assert!(preview.entries[0].reason.contains("8 KiB"));
    }

    #[test]
    fn coverage_rows_reclassify_eligible_as_unchecked() {
        let preview = parse_paste(
            "https://example.com/a\nhttps://example.com/a\nftp://example.com/\n",
            &scope(),
        )
        .expect("preview");
        let rows = preview.coverage_rows();
        assert_eq!(rows[0].outcome, Outcome::Unchecked);
        assert_eq!(rows[1].outcome, Outcome::Duplicate);
        assert_eq!(rows[2].outcome, Outcome::Invalid);
        assert_eq!(
            csv_row(&rows[0]),
            vec![
                "1",
                "https://example.com/a",
                "https://example.com/a",
                "Unchecked",
                "Queued for checking",
                "",
                "",
                ""
            ]
        );
    }
}
