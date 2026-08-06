//! MySQL client (mysql) output compression.
//!
//! Detects table-mode output (`+----+` box borders, as produced by `-t` or the
//! interactive default), strips the borders, padding, and `... (N.NN sec)`
//! footer, and delegates row de-formatting to the shared [`strip_ascii_table`].
//! Batch mode (`-B`, already tab-separated) and vertical `\G` output are passed
//! through unchanged.
//!
//! Credential safety: the child command receives every argument untouched (so
//! `--defaults-file` and any auth flags work), but the label handed to the
//! runner for tracking/logging is scrubbed — an inline `-p<password>` or
//! `--password=<value>` is never persisted. See [`redact_credentials`].
//!
//! # Example
//!
//! Raw `mysql -t` table output:
//!
//! ```text
//! +----+-------------+-------------------+--------+
//! | id | username    | email             | status |
//! +----+-------------+-------------------+--------+
//! |  1 | alice_smith | alice@example.com | active |
//! |  2 | bob_jones   | bob@example.com   | active |
//! +----+-------------+-------------------+--------+
//! 2 rows in set (0.00 sec)
//! ```
//!
//! Compressed to tab-separated rows (`\t` marks each tab; borders and footer gone):
//!
//! ```text
//! id\tusername\temail\tstatus
//! 1\talice_smith\talice@example.com\tactive
//! 2\tbob_jones\tbob@example.com\tactive
//! ```

use crate::core::ascii_table::{strip_ascii_table, TableShape};
use crate::core::runner::{self, RunOptions};
use crate::core::utils::resolved_command;
use anyhow::Result;
use regex::Regex;
use std::sync::LazyLock;

/// A mysql box-table border: `+----+------+` (starts and ends with `+`).
static TABLE_BORDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\+[-+]+\+$").unwrap());
/// The timing footer shared by every mysql status line:
/// `N rows in set (0.00 sec)`, `Empty set (0.00 sec)`,
/// `Query OK, N rows affected (0.00 sec)`, `... 1 warning (0.00 sec)`.
/// Also tolerates a comma decimal separator in localized builds.
///
/// Anchored to end-of-line: mysql always closes the status line with the timing
/// parenthesis, so an unanchored match would also swallow unrelated lines that
/// merely *mention* a duration.
static FOOTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(\d+[.,]\d+ sec\)$").unwrap());

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("mysql");
    for arg in args {
        // Every argument is forwarded untouched so `--defaults-file`, auth
        // flags, `-e`, etc. behave exactly as they would for raw `mysql`.
        cmd.arg(arg);
    }

    // Scrubbed for anything that gets logged or persisted — never the raw args.
    let display = redact_credentials(args);

    if verbose > 0 {
        eprintln!("Running: mysql {}", display);
    }

    runner::run_filtered(
        cmd,
        "mysql",
        &display,
        filter_mysql_output,
        RunOptions::stdout_only().tee("mysql").early_exit_on_failure(),
    )
}

/// Build a display string with inline credentials redacted. Only the *label*
/// is affected — the executed command still receives the real values.
///
/// MySQL only accepts a password *attached* to the flag (`-pSECRET`,
/// `--password=SECRET`); a bare `-p`/`--password` prompts interactively and a
/// space-separated token is treated as a database name, so a single-arg scan is
/// sufficient. `-P` (uppercase, port) and `--defaults-file` are left intact.
fn redact_credentials(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if let Some(rest) = arg.strip_prefix("--password=") {
                if rest.is_empty() {
                    arg.clone()
                } else {
                    "--password=***".to_string()
                }
            } else if arg.starts_with("-p") && arg.len() > 2 {
                "-p***".to_string()
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn filter_mysql_output(output: &str) -> String {
    if output.trim().is_empty() {
        return String::new();
    }

    if is_table_format(output) {
        filter_table(output)
    } else {
        // Batch mode (`-B`, already TSV), vertical `\G`, `Query OK` status
        // lines, notices — nothing to compress, pass through untouched.
        output.to_string()
    }
}

fn is_table_format(output: &str) -> bool {
    output.lines().any(|line| TABLE_BORDER.is_match(line.trim()))
}

/// Is this line the trailing `N rows in set (0.00 sec)` status line?
///
/// A cell value can legitimately contain a `(N.NN sec)`-shaped substring, so
/// matching the timing pattern alone would silently delete data rows. mysql
/// wraps every table row in `|` bars and never puts one in a status line, so
/// the bar is the discriminator.
fn is_footer_line(line: &str) -> bool {
    let line = line.trim();
    !line.contains('|') && FOOTER.is_match(line)
}

/// Drop the `(N.NN sec)` footer, then hand the rest to the shared ASCII-table
/// stripper. mysql wraps every row in outer bars, so `has_outer_pipes` is true.
fn filter_table(output: &str) -> String {
    let body = output
        .lines()
        .filter(|line| !is_footer_line(line))
        .collect::<Vec<_>>()
        .join("\n");

    strip_ascii_table(
        &body,
        TableShape {
            has_outer_pipes: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn test_table_format_stripped() {
        let input = "\
+----+-------------+-------------------+--------+
| id | username    | email             | status |
+----+-------------+-------------------+--------+
|  1 | alice_smith | alice@example.com | active |
|  2 | bob_jones   | bob@example.com   | active |
+----+-------------+-------------------+--------+
2 rows in set (0.00 sec)";
        let out = filter_mysql_output(input);
        assert_eq!(
            out,
            "id\tusername\temail\tstatus\n\
             1\talice_smith\talice@example.com\tactive\n\
             2\tbob_jones\tbob@example.com\tactive"
        );
        assert!(!out.contains('+'));
        assert!(!out.contains("2 rows in set"));
    }

    #[test]
    fn test_footer_variants_stripped() {
        for footer in [
            "2 rows in set (0.00 sec)",
            "Empty set (0.00 sec)",
            "1 row in set, 1 warning (0.01 sec)",
        ] {
            let input = format!("+----+\n| id |\n+----+\n|  1 |\n+----+\n{}", footer);
            let out = filter_mysql_output(&input);
            assert!(!out.contains("sec)"), "footer leaked: {}", footer);
        }
    }

    #[test]
    fn test_footer_shaped_cell_content_preserved() {
        // Regression: an unanchored, pipe-blind footer match deleted any data
        // row whose cell text happened to contain `(N.NN sec)`.
        let input = "\
+----+------------------------------+
| id | note                         |
+----+------------------------------+
|  1 | backup done (12.34 sec) ok   |
|  2 | second row                   |
+----+------------------------------+
2 rows in set (0.00 sec)";
        let out = filter_mysql_output(input);
        assert_eq!(
            out,
            "id\tnote\n1\tbackup done (12.34 sec) ok\n2\tsecond row"
        );
        assert!(!out.contains("2 rows in set"));
    }

    #[test]
    fn test_footer_shaped_cell_at_row_end_preserved() {
        // Worst case: the timing substring is the last thing on the line before
        // the closing bar, so even an end-anchored match must not fire.
        let input = "\
+----+---------------------+
| id | note                |
+----+---------------------+
|  1 | done (12.34 sec)    |
+----+---------------------+
1 row in set (0.00 sec)";
        let out = filter_mysql_output(input);
        assert_eq!(out, "id\tnote\n1\tdone (12.34 sec)");
    }

    #[test]
    fn test_is_footer_line() {
        assert!(is_footer_line("2 rows in set (0.00 sec)"));
        assert!(is_footer_line("Query OK, 1 row affected (0,01 sec)"));
        assert!(!is_footer_line("|  1 | backup done (12.34 sec) ok |"));
        // Mentions a duration but is not a status line.
        assert!(!is_footer_line(
            "ERROR 1205 (HY000): Lock wait timeout (51.00 sec) exceeded"
        ));
    }

    #[test]
    fn test_batch_mode_passthrough() {
        // `-B` output is already tab-separated with no borders — leave it alone.
        let input = "id\tusername\temail\n1\talice_smith\talice@example.com\n2\tbob_jones\tbob@example.com";
        let out = filter_mysql_output(input);
        assert_eq!(out, input);
    }

    #[test]
    fn test_vertical_passthrough() {
        // `\G` vertical output is not table-mode; deferred to a follow-up.
        let input = "*************************** 1. row ***************************\n      id: 1\nusername: alice_smith";
        let out = filter_mysql_output(input);
        assert_eq!(out, input);
    }

    #[test]
    fn test_interior_null_preserved() {
        let input = "+----+------+------+\n| id | name | note |\n+----+------+------+\n|  1 | foo  |      |\n+----+------+------+\n1 row in set (0.00 sec)";
        let out = filter_mysql_output(input);
        // The empty NULL cell must survive as an empty tab-delimited field.
        assert!(out.contains("1\tfoo\t"));
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(filter_mysql_output(""), "");
    }

    #[test]
    fn test_is_table_format() {
        assert!(is_table_format("+----+\n| id |\n+----+"));
        assert!(!is_table_format("id\tname\n1\tfoo"));
        assert!(!is_table_format("Query OK, 1 row affected (0.00 sec)"));
    }

    #[test]
    fn test_token_savings() {
        let input = "\
+----+-------------+-------------------+--------+
| id | username    | email             | status |
+----+-------------+-------------------+--------+
|  1 | alice_smith | alice@example.com | active |
|  2 | bob_jones   | bob@example.com   | active |
+----+-------------+-------------------+--------+
2 rows in set (0.00 sec)";
        let out = filter_mysql_output(input);
        let savings = 100.0 - (count_tokens(&out) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(savings >= 60.0, "expected >=60% savings, got {:.1}%", savings);
    }

    // --- credential safety ---

    #[test]
    fn test_redact_inline_password() {
        let args = vec!["-t".to_string(), "-pSecret123".to_string()];
        let display = redact_credentials(&args);
        assert_eq!(display, "-t -p***");
        assert!(!display.contains("Secret123"));
    }

    #[test]
    fn test_redact_password_flag() {
        let args = vec!["--password=hunter2".to_string()];
        let display = redact_credentials(&args);
        assert_eq!(display, "--password=***");
        assert!(!display.contains("hunter2"));
    }

    #[test]
    fn test_defaults_file_preserved() {
        let args = vec![
            "--defaults-file=/tmp/.my.cnf".to_string(),
            "-t".to_string(),
            "-e".to_string(),
            "SELECT 1".to_string(),
        ];
        let display = redact_credentials(&args);
        // A path is not a credential — it must remain intact for readability.
        assert!(display.contains("--defaults-file=/tmp/.my.cnf"));
        assert!(display.contains("SELECT 1"));
    }

    #[test]
    fn test_non_credential_flags_untouched() {
        // Bare `-p` (prompt), `-P` (port), `--password` (prompt) are not secrets.
        let args = vec![
            "-p".to_string(),
            "-P3306".to_string(),
            "--password".to_string(),
            "-uroot".to_string(),
        ];
        let display = redact_credentials(&args);
        assert_eq!(display, "-p -P3306 --password -uroot");
    }
}
