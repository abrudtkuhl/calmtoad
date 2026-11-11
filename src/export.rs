//! Helpers for exporting crawl results in spreadsheet-friendly CSV format.

/// Encode rows as UTF-8 CSV. Every field is quoted so commas, quotes, Unicode, and embedded
/// line breaks round-trip cleanly. A leading apostrophe protects spreadsheet users from formula
/// injection in crawled content while leaving ordinary values unchanged.
pub fn to_csv(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut output = String::new();
    write_record(&mut output, headers.iter().copied());
    for row in rows {
        write_record(&mut output, row.iter().map(String::as_str));
    }
    output
}

fn write_record<'a>(output: &mut String, fields: impl IntoIterator<Item = &'a str>) {
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        if starts_with_formula_marker(field) {
            output.push('\'');
        }
        for ch in field.chars() {
            if ch == '"' {
                output.push('"');
            }
            output.push(ch);
        }
        output.push('"');
    }
    output.push_str("\r\n");
}

fn starts_with_formula_marker(value: &str) -> bool {
    matches!(
        value
            .chars()
            .find(|ch| !ch.is_whitespace() && !ch.is_control() && *ch != '\u{feff}'),
        Some('=' | '+' | '-' | '@')
    )
}

#[cfg(test)]
mod tests {
    use super::to_csv;

    #[test]
    fn quotes_fields_and_preserves_commas_quotes_newlines_and_unicode() {
        let csv = to_csv(
            &["Address", "Title", "Description"],
            &[vec![
                "https://example.test/a,b".into(),
                "A \"quoted\" title".into(),
                "first line\nsecond line — 日本語".into(),
            ]],
        );

        assert_eq!(
            csv,
            concat!(
                "\"Address\",\"Title\",\"Description\"\r\n",
                "\"https://example.test/a,b\",\"A \"\"quoted\"\" title\",",
                "\"first line\nsecond line — 日本語\"\r\n"
            )
        );
    }

    #[test]
    fn protects_spreadsheet_formula_markers_after_whitespace_and_controls() {
        let csv = to_csv(
            &["value"],
            &[
                vec!["=1+1".into()],
                vec![" +SUM(A1:A2)".into()],
                vec!["\t@HYPERLINK(\"https://evil.test\")".into()],
                vec!["-1".into()],
                vec!["ordinary text".into()],
                vec![String::new()],
            ],
        );

        assert_eq!(
            csv,
            concat!(
                "\"value\"\r\n\"'=1+1\"\r\n\"' +SUM(A1:A2)\"\r\n",
                "\"'\t@HYPERLINK(\"\"https://evil.test\"\")\"\r\n",
                "\"'-1\"\r\n\"ordinary text\"\r\n\"\"\r\n"
            )
        );
    }
}
