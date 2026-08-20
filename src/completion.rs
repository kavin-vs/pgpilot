//! Pure completion-candidate logic for the Playground's Tab-triggered
//! autocomplete (see `main.rs::playground_autocomplete`) — no I/O, so it's
//! independently testable without a database.

use crate::db::tables::TablesData;

/// A small hand-rolled list of common SQL keywords — no reserved-keyword
/// list exists elsewhere in this codebase to reuse (only narrow 1-5-item
/// prefix checks in `db::playground` for a different purpose).
/// ponytail: not exhaustive, just the common ones a query actually needs.
const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "INSERT", "INTO", "UPDATE", "DELETE", "SET", "VALUES", "JOIN", "INNER", "LEFT",
    "RIGHT", "OUTER", "ON", "GROUP BY", "ORDER BY", "LIMIT", "OFFSET", "HAVING", "AND", "OR", "NOT", "NULL", "IS",
    "IN", "LIKE", "ILIKE", "AS", "DISTINCT", "CREATE TABLE", "DROP", "ALTER", "INDEX", "VIEW", "WITH", "UNION",
    "ALL", "EXISTS", "BEGIN", "COMMIT", "ROLLBACK", "RETURNING", "CASE", "WHEN", "THEN", "ELSE", "END", "TRUE",
    "FALSE", "ASC", "DESC",
];

/// Caps how many candidates a single Tab press ever cycles through — plenty
/// for any real schema, keeps the cycle from growing unbounded.
const MAX_CANDIDATES: usize = 50;

/// Wraps `name` in double quotes (escaping embedded `"` by doubling — real
/// Postgres identifier-quoting rules) only if it isn't already a plain,
/// unquoted-safe identifier (`^[a-z_][a-z0-9_]*$`) — the "valid format" part
/// of autocomplete: whatever gets inserted is always safe to run as-is.
pub fn quote_ident(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Scans backward from `cursor_col` (a char index into `line`) over
/// identifier/`.` characters to find the partial word being typed — `.` is
/// included so a typed `schema.tab` prefix comes back whole (`candidates`
/// splits it on the last `.` to filter schema/table separately). Returns
/// the word's starting char column and the word text itself.
pub fn word_before_cursor(line: &str, cursor_col: usize) -> (usize, String) {
    let chars: Vec<char> = line.chars().collect();
    let end = cursor_col.min(chars.len());
    let mut start = end;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_' || chars[start - 1] == '.') {
        start -= 1;
    }
    (start, chars[start..end].iter().collect())
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
}

/// Builds the Tab-completion candidate pool for `prefix` (the partial word
/// just before the cursor, from `word_before_cursor`): SQL keywords, plus —
/// when `tables` is available — every table name (unqualified) and, for
/// non-`public` schemas, the `schema.table` qualified form too (skipped for
/// `public` since qualifying it is just noise). Each identifier is passed
/// through `quote_ident` so whatever's inserted is always syntactically
/// valid. Matching is a case-insensitive prefix check — no fuzzy matching,
/// consistent with the rest of this app's naive-heuristic style. A typed
/// `schema.tab` prefix is split on its last `.` and matched against schema
/// and table names separately.
///
/// ponytail: `tables` reflects the slow poll tier's up-to-5-minute-stale
/// snapshot (same staleness `App::tables_rates` already documents) — fine
/// for autocomplete, not worth a dedicated fetch. Column-name completion is
/// out of scope — no existing data source beyond the narrow FK-column list.
pub fn candidates(prefix: &str, tables: Option<&TablesData>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    if let Some((schema_prefix, table_prefix)) = prefix.rsplit_once('.') {
        if let Some(data) = tables {
            for t in &data.tables {
                if starts_with_ci(&t.schema_name, schema_prefix) && starts_with_ci(&t.table_name, table_prefix) {
                    out.push(format!("{}.{}", quote_ident(&t.schema_name), quote_ident(&t.table_name)));
                }
            }
        }
    } else {
        for kw in KEYWORDS {
            if starts_with_ci(kw, prefix) {
                out.push(kw.to_string());
            }
        }
        if let Some(data) = tables {
            for t in &data.tables {
                if starts_with_ci(&t.table_name, prefix) {
                    out.push(quote_ident(&t.table_name));
                }
                if t.schema_name != "public" && starts_with_ci(&t.schema_name, prefix) {
                    out.push(format!("{}.{}", quote_ident(&t.schema_name), quote_ident(&t.table_name)));
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out.truncate(MAX_CANDIDATES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tables::TableRow;
    use std::collections::BTreeMap;

    fn sample_table(schema: &str, table: &str) -> TableRow {
        TableRow {
            schema_name: schema.to_string(),
            table_name: table.to_string(),
            total_bytes: 0,
            indexes_toast_bytes: 0,
            live_tuples: 0,
            dead_tuple_pct: None,
            xid_age: 0,
            seq_scan: 0,
            idx_use_pct: None,
            last_vacuum_secs_ago: None,
        }
    }

    fn sample_tables() -> TablesData {
        TablesData {
            tables: vec![
                sample_table("public", "users"),
                sample_table("public", "user_sessions"),
                sample_table("Audit", "log"),
            ],
            schema_totals: BTreeMap::new(),
        }
    }

    #[test]
    fn word_before_cursor_extracts_mid_line_prefix() {
        assert_eq!(word_before_cursor("select * from us", 16), (14, "us".to_string()));
    }

    #[test]
    fn word_before_cursor_keeps_dotted_schema_prefix() {
        assert_eq!(word_before_cursor("select * from aud.lo", 20), (14, "aud.lo".to_string()));
    }

    #[test]
    fn quote_ident_leaves_plain_names_unquoted() {
        assert_eq!(quote_ident("users"), "users");
    }

    #[test]
    fn quote_ident_quotes_and_escapes_mixed_case_names() {
        assert_eq!(quote_ident("MyTable"), "\"MyTable\"");
        assert_eq!(quote_ident("weird\"name"), "\"weird\"\"name\"");
    }

    #[test]
    fn candidates_matches_keyword_prefix() {
        let out = candidates("sel", None);
        assert!(out.contains(&"SELECT".to_string()));
    }

    #[test]
    fn candidates_matches_unqualified_table_name() {
        let data = sample_tables();
        let out = candidates("us", Some(&data));
        assert!(out.contains(&"users".to_string()));
        assert!(out.contains(&"user_sessions".to_string()));
    }

    #[test]
    fn candidates_matches_schema_qualified_table_for_non_public_schema() {
        let data = sample_tables();
        let out = candidates("aud.l", Some(&data));
        assert_eq!(out, vec!["\"Audit\".log".to_string()]);
    }
}
