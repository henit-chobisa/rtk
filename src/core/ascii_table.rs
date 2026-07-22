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

lazy_static! {
    static ref BORDER: Regex = Regex::new(r"^[-+]+$").unwrap();
}

pub struct TableShape {
    /// Rows are wrapped in outer bars (`| a | b |`) as in mysql. psql tables
    /// have no outer bars, so this is `false` for them.
    pub has_outer_pipes: bool,
}

/// De-decorate an ASCII table body. The first row containing `|` is treated as
/// the header (always kept); data rows are capped at [`CAP_LIST`], with an
/// `... +N more rows` marker when truncated.
pub fn strip_ascii_table(body: &str, shape: TableShape) -> String {
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
                let cells: Vec<&str> = trimmed.split('|').map(|c| c.trim()).collect();
                let joined = if shape.has_outer_pipes {
                    // Drop the empty edge cells produced by the outer bars.
                    // Interior empty cells (NULLs) are preserved.
                    cells[1..cells.len() - 1].join("\t")
                } else {
                    cells.join("\t")
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
