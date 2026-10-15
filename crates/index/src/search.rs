//! Searching: FTS5 `MATCH` + `bm25()` with per-column and per-role weights, one hit per session.

use rusqlite::Connection;
use rusqlite::types::Value;

use crate::error::{IndexError, Result};
use crate::types::{BlockRef, Hit, Role, SNIPPET_CLOSE, SNIPPET_OPEN, SearchQuery, Source};

/// `bm25` weights of the `body` and `idents` columns: a match on an identifier (path, flag,
/// command) counts twice as much as the same word in prose.
pub const BODY_WEIGHT: f64 = 1.0;
pub const IDENTS_WEIGHT: f64 = 2.0;
/// Most terms of one query.
const MAX_TERMS: usize = 32;
/// Words of context in a snippet.
const SNIPPET_WORDS: u32 = 16;

/// Turns what the user typed into an FTS5 expression: every word, and every `"quoted phrase"`,
/// becomes one quoted phrase; all must match. Words without a letter or digit are dropped (they
/// tokenize to nothing).
pub fn match_expression(text: &str) -> Result<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut term = String::new();
        if c == '"' {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                term.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                term.push(c);
                chars.next();
            }
        }
        if term.chars().any(char::is_alphanumeric) {
            terms.push(format!("\"{}\"", term.replace('"', "\"\"")));
        }
    }
    if terms.is_empty() {
        return Err(IndexError::BadQuery(
            "the query has no words to search for".to_owned(),
        ));
    }
    terms.truncate(MAX_TERMS);
    Ok(terms.join(" "))
}

fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn search(conn: &Connection, query: &SearchQuery) -> Result<Vec<Hit>> {
    let expression = match_expression(&query.text)?;
    let limit = query.limit.clamp(1, 1000);

    let mut params: Vec<Value> = vec![Value::Real(BODY_WEIGHT), Value::Real(IDENTS_WEIGHT)];
    let mut role_cases = String::new();
    for role in Role::ALL {
        role_cases.push_str(&format!("WHEN '{}' THEN ? ", role.as_str()));
        params.push(Value::Real(role.weight()));
    }
    params.push(Value::Text(expression.clone()));

    let mut filters = String::new();
    if !query.sources.is_empty() {
        let marks = vec!["?"; query.sources.len()].join(",");
        filters.push_str(&format!(" AND s.source IN ({marks})"));
        params.extend(
            query
                .sources
                .iter()
                .map(|s| Value::Text(s.as_str().to_owned())),
        );
    }
    if !query.roles.is_empty() {
        let marks = vec!["?"; query.roles.len()].join(",");
        filters.push_str(&format!(" AND e.role IN ({marks})"));
        params.extend(
            query
                .roles
                .iter()
                .map(|r| Value::Text(r.as_str().to_owned())),
        );
    }
    if let Some(project) = query.project.as_deref().filter(|p| !p.is_empty()) {
        let project = project.trim_end_matches('/');
        filters.push_str(" AND (s.cwd = ? OR s.cwd LIKE ? ESCAPE '\\')");
        params.push(Value::Text(project.to_owned()));
        params.push(Value::Text(format!("{}/%", like_escape(project))));
    }
    if let Some(since) = query.since_ms {
        filters.push_str(" AND e.ts_ms >= ?");
        params.push(Value::Integer(i64::try_from(since).unwrap_or(i64::MAX)));
    }
    if let Some(until) = query.until_ms {
        filters.push_str(" AND e.ts_ms <= ?");
        params.push(Value::Integer(i64::try_from(until).unwrap_or(i64::MAX)));
    }
    params.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));

    // `bm25` is negative and smaller is better; role weights above 1 pull the score down.
    let sql = format!(
        "WITH hits AS (
             SELECT e.id AS id, e.session_id AS session_id, e.role AS role, e.ts_ms AS ts_ms,
                    e.terminal_id AS terminal_id, e.block_id AS block_id,
                    bm25(fts, ?, ?) * (CASE e.role {role_cases} ELSE 1.0 END) AS score
             FROM fts
             JOIN entries e ON e.id = fts.rowid
             JOIN sessions s ON s.id = e.session_id
             WHERE fts MATCH ?{filters}
         ), ranked AS (
             SELECT *,
                    ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY score ASC, ts_ms DESC, id DESC) AS rn,
                    COUNT(*) OVER (PARTITION BY session_id) AS n
             FROM hits
         )
         SELECT r.id, r.score, r.n, r.role, r.ts_ms, s.source, s.key, s.title, s.cwd,
                src.path, r.terminal_id, r.block_id
         FROM ranked r
         JOIN sessions s ON s.id = r.session_id
         LEFT JOIN sources src ON src.id = s.source_id
         WHERE r.rn = 1
         ORDER BY r.score ASC, r.ts_ms DESC
         LIMIT ?"
    );
    let context = "searching the history index";
    let mut stmt = conn.prepare(&sql).map_err(IndexError::sqlite(context))?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(params))
        .map_err(sqlite_or_bad_query(context))?;
    let mut found = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_or_bad_query(context))? {
        let get = IndexError::sqlite(context);
        let id: i64 = row.get(0).map_err(get)?;
        let score: f64 = row.get(1).map_err(IndexError::sqlite(context))?;
        let matches: i64 = row.get(2).map_err(IndexError::sqlite(context))?;
        let role: String = row.get(3).map_err(IndexError::sqlite(context))?;
        let ts_ms: Option<i64> = row.get(4).map_err(IndexError::sqlite(context))?;
        let source: String = row.get(5).map_err(IndexError::sqlite(context))?;
        let terminal_id: Option<i64> = row.get(10).map_err(IndexError::sqlite(context))?;
        let block_id: Option<i64> = row.get(11).map_err(IndexError::sqlite(context))?;
        found.push((
            id,
            Hit {
                source: source.parse::<Source>().unwrap_or(Source::Claude),
                session_key: row.get(6).map_err(IndexError::sqlite(context))?,
                title: row.get(7).map_err(IndexError::sqlite(context))?,
                cwd: row.get(8).map_err(IndexError::sqlite(context))?,
                role: role.parse().unwrap_or(Role::Tool),
                ts_ms: ts_ms.and_then(|t| u64::try_from(t).ok()),
                snippet: String::new(),
                score: -score,
                matches: u32::try_from(matches).unwrap_or(u32::MAX),
                path: row.get(9).map_err(IndexError::sqlite(context))?,
                block: terminal_id.zip(block_id).and_then(|(terminal, block)| {
                    Some(BlockRef {
                        terminal_id: u64::try_from(terminal).ok()?,
                        block_id: u64::try_from(block).ok()?,
                    })
                }),
            },
        ));
    }
    drop(rows);
    drop(stmt);

    let mut snippet = conn
        .prepare(&format!(
            "SELECT snippet(fts, -1, '{SNIPPET_OPEN}', '{SNIPPET_CLOSE}', '…', {SNIPPET_WORDS}) FROM fts WHERE fts MATCH ?1 AND rowid = ?2"
        ))
        .map_err(IndexError::sqlite(context))?;
    let mut hits = Vec::with_capacity(found.len());
    for (id, mut hit) in found {
        hit.snippet = snippet
            .query_row(rusqlite::params![expression, id], |row| row.get(0))
            .map_err(IndexError::sqlite(context))?;
        hits.push(hit);
    }
    Ok(hits)
}

/// FTS5 reports a malformed expression as a plain SQLite error; with the quoting above that
/// should not happen, but when it does the user gets a query error rather than a database one.
fn sqlite_or_bad_query(context: &'static str) -> impl FnOnce(rusqlite::Error) -> IndexError {
    move |source| match &source {
        rusqlite::Error::SqliteFailure(_, Some(message)) if message.contains("fts5") => {
            IndexError::BadQuery(format!("invalid query: {message}"))
        }
        _ => IndexError::sqlite(context)(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_phrases_become_quoted_terms() {
        assert_eq!(
            match_expression("cargo test -p daemon").unwrap(),
            r#""cargo" "test" "-p" "daemon""#
        );
        assert_eq!(
            match_expression(r#"say "hello world" now"#).unwrap(),
            r#""say" "hello world" "now""#
        );
        // Operators and syntax characters are plain text.
        assert_eq!(
            match_expression("a OR b NEAR(c) col:x*").unwrap(),
            r#""a" "OR" "b" "NEAR(c)" "col:x*""#
        );
        assert_eq!(match_expression(r#"un"closed"#).unwrap(), r#""un""closed""#);
    }

    #[test]
    fn a_query_without_words_is_rejected() {
        for text in ["", "   ", "-- ** !!", "\"\""] {
            assert!(
                matches!(match_expression(text), Err(IndexError::BadQuery(_))),
                "{text:?}"
            );
        }
    }
}
