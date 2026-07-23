//! Strips ASCII table decoration (box borders and cell padding) into compact
//! tab-separated rows. Generic: it knows nothing about SQL. Footer lines and
//! any format-specific quirks are the caller's responsibility to strip before
//! calling here.
//!
//! # Example
//!
//! Raw table body (borders + alignment padding):
//!
//! ```text
//! +----+-------------+-------------------+--------+
//! | id | username    | email             | status |
//! +----+-------------+-------------------+--------+
//! |  1 | alice_smith | alice@example.com | active |
//! |  2 | bob_jones   | bob@example.com   | active |
//! +----+-------------+-------------------+--------+
//! ```
//!
//! Compressed to tab-separated rows (`\t` marks each tab; borders and padding gone):
//!
//! ```text
//! id\tusername\temail\tstatus
//! 1\talice_smith\talice@example.com\tactive
//! 2\tbob_jones\tbob@example.com\tactive
//! ```

use crate::core::truncate::CAP_LIST;
use lazy_static::lazy_static;
use regex::Regex;

/// Column junction character in the border row (`----+----`). The paired cell
/// separator is [`SEPARATOR`]. Both follow the near-universal ASCII box-table
/// convention; if a dialect ever differs, promote these to fields on
/// [`TableShape`] with that consumer as justification.
const JUNCTION: char = '+';
/// Cell separator character in data rows (`| a | b |`).
const SEPARATOR: u8 = b'|';

lazy_static! {
    static ref BORDER: Regex = Regex::new(r"^[-+]+$").unwrap();
}

pub struct TableShape {
    /// Rows are wrapped in outer bars (`| a | b |`) as in mysql. psql tables
    /// have no outer bars, so this is `false` for them.
    pub has_outer_pipes: bool,
}

/// Byte positions of the column junctions (`+`) in the first border row.
/// Returns `[]` when no `+`-bearing border exists, which disables offset
/// slicing and falls the caller back to naive splitting.
fn junction_offsets(body: &str) -> Vec<usize> {
    body.lines()
        .find(|l| BORDER.is_match(l.trim()) && l.contains(JUNCTION))
        .map(|border| border.match_indices(JUNCTION).map(|(i, _)| i).collect())
        .unwrap_or_default()
}

/// Slice a data row at the fixed column boundaries derived from the border,
/// so a `|` *inside* a cell value (config strings, stored regexes) is never
/// mistaken for a column separator.
///
/// Returns `None` — signalling the caller to fall back to naive splitting —
/// when the row doesn't carry a `|` at every junction offset. That happens for
/// borderless input or when a multi-byte cell shifts the byte offsets; the
/// fallback already handles those correctly (they have no interior pipes).
fn split_by_offsets(line: &str, junctions: &[usize], has_outer_pipes: bool) -> Option<Vec<String>> {
    if junctions.is_empty() {
        return None;
    }
    // Every junction offset must line up with a `|` in this row.
    if !junctions
        .iter()
        .all(|&i| line.as_bytes().get(i) == Some(&SEPARATOR))
    {
        return None;
    }

    let cells: Vec<String> = if has_outer_pipes {
        // The outer `+` are the bars; columns are the interior windows. The
        // slice bounds (`j + 1` and `j`) are ASCII `|` positions, so they are
        // always valid char boundaries.
        if junctions.len() < 2 {
            return None;
        }
        junctions
            .windows(2)
            .map(|w| line[w[0] + 1..w[1]].trim().to_string())
            .collect()
    } else {
        // No outer bars: add virtual edges at 0 and end around the interior
        // junctions. The first cell starts at 0 (content, not a separator).
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut start = 0usize;
        for &j in junctions {
            spans.push((start, j));
            start = j + 1;
        }
        spans.push((start, line.len()));
        spans
            .into_iter()
            .map(|(s, e)| line[s..e].trim().to_string())
            .collect()
    };
    Some(cells)
}

/// De-decorate an ASCII table body. The first row containing `|` is treated as
/// the header (always kept); data rows are capped at [`CAP_LIST`], with an
/// `... +N more rows` marker when truncated.
pub fn strip_ascii_table(body: &str, shape: TableShape) -> String {
    let junctions = junction_offsets(body);

    let mut out: Vec<String> = Vec::new();
    let mut pipe_rows = 0usize; // header + data rows encountered
    let mut data_rows = 0usize; // data rows only (header excluded)

    for line in body.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Skip border lines: `----+----` (psql) and `+----+----+` (mysql).
        if BORDER.is_match(trimmed) {
            continue;
        }

        if trimmed.contains('|') {
            pipe_rows += 1;
            let is_header = pipe_rows == 1;
            if !is_header {
                data_rows += 1;
            }

            // Keep the header and the first CAP_LIST data rows; drop the rest.
            if is_header || data_rows <= CAP_LIST {
                let joined = match split_by_offsets(line, &junctions, shape.has_outer_pipes) {
                    Some(cells) => cells.join("\t"),
                    None => naive_split(trimmed, shape.has_outer_pipes),
                };
                out.push(joined);
            }
        } else {
            // Non-table line the caller left in (notice, etc.) — pass through.
            out.push(trimmed.to_string());
        }
    }

    if data_rows > CAP_LIST {
        out.push(format!("... +{} more rows", data_rows - CAP_LIST));
    }

    out.join("\n")
}

/// Fallback used when the border-offset slice can't apply: split on every `|`.
/// Correct for borderless tables and unicode rows without interior pipes.
fn naive_split(trimmed: &str, has_outer_pipes: bool) -> String {
    let cells: Vec<&str> = trimmed.split('|').map(|c| c.trim()).collect();
    if has_outer_pipes && cells.len() >= 2 {
        // Drop the empty edge cells produced by the outer bars. Interior empty
        // cells (NULLs) are preserved.
        cells[1..cells.len() - 1].join("\t")
    } else {
        cells.join("\t")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn test_outer_pipes_mysql_style() {
        let input = "\
+----+-------------+
| id | username    |
+----+-------------+
|  1 | alice_smith |
|  2 | bob_jones   |
+----+-------------+";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: true,
            },
        );
        assert_eq!(out, "id\tusername\n1\talice_smith\n2\tbob_jones");
    }

    #[test]
    fn test_no_outer_pipes_psql_style() {
        let input = " id | username\n----+-------------\n  1 | alice_smith\n  2 | bob_jones";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: false,
            },
        );
        assert_eq!(out, "id\tusername\n1\talice_smith\n2\tbob_jones");
    }

    #[test]
    fn test_pipe_inside_cell_not_split() {
        // A `|` inside a cell value must stay in that cell, not become a column
        // boundary. Border offsets pin the real columns; interior pipes never
        // sit at a junction. Regression for the split('|') bug.
        let input = "\
+----+-------+
| id | val   |
+----+-------+
| 1  | a|b|c |
+----+-------+";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: true,
            },
        );
        assert_eq!(out, "id\tval\n1\ta|b|c");
    }

    #[test]
    fn test_regex_inside_cell_not_split() {
        let input = "\
+----+-----------------+
| id | pattern         |
+----+-----------------+
| 1  | ^(foo|bar|baz)$ |
+----+-----------------+";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: true,
            },
        );
        assert_eq!(out, "id\tpattern\n1\t^(foo|bar|baz)$");
    }

    #[test]
    fn test_pipe_inside_cell_psql_style() {
        // Same guarantee for the no-outer-bars (psql) offset path.
        let input = " id | pattern\n----+-------------\n  1 | a|b|c";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: false,
            },
        );
        assert_eq!(out, "id\tpattern\n1\ta|b|c");
    }

    #[test]
    fn test_unicode_cell_falls_back_cleanly() {
        // Multi-byte content shifts the byte offsets so the junction check
        // fails on that row → naive-split fallback. The cell has no interior
        // pipe, so the fallback is correct; must not panic or corrupt.
        let input = "\
+----+--------+
| id | name   |
+----+--------+
| 1  | café☕ |
+----+--------+";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: true,
            },
        );
        assert_eq!(out, "id\tname\n1\tcafé☕");
    }

    #[test]
    fn test_interior_null_preserved() {
        // Middle cell is an empty NULL — must survive, only edges dropped.
        let input = "| 1 |  | c |";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: true,
            },
        );
        assert_eq!(out, "1\t\tc");
    }

    #[test]
    fn test_row_cap_and_overflow() {
        let mut lines = vec!["| id | val |".to_string()];
        for i in 1..=CAP_LIST + 5 {
            lines.push(format!("| {} | v{} |", i, i));
        }
        let input = lines.join("\n");
        let out = strip_ascii_table(
            &input,
            TableShape {
                has_outer_pipes: true,
            },
        );

        assert!(out.contains("... +5 more rows"));
        // header + CAP_LIST data rows + overflow marker
        assert_eq!(out.lines().count(), CAP_LIST + 2);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(
            strip_ascii_table(
                "",
                TableShape {
                    has_outer_pipes: true
                }
            ),
            ""
        );
    }

    #[test]
    fn test_token_savings() {
        let input = "\
+----+-------------+-------------------+--------+
| id | username    | email             | status |
+----+-------------+-------------------+--------+
|  1 | alice_smith | alice@example.com | active |
|  2 | bob_jones   | bob@example.com   | active |
+----+-------------+-------------------+--------+";
        let out = strip_ascii_table(
            input,
            TableShape {
                has_outer_pipes: true,
            },
        );
        let savings = 100.0 - (count_tokens(&out) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            savings >= 40.0,
            "expected >=40% savings, got {:.1}%",
            savings
        );
    }
}
