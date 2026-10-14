//! Turning raw text into index rows: chunking and the cheap identifier tokenizer.

/// Most bytes of one row's `body`.
pub const CHUNK_BYTES: usize = 4096;
/// Most bytes of one row's `idents`.
const IDENTS_BYTES: usize = 1024;
/// Most identifiers kept per row.
const IDENTS_MAX: usize = 64;
/// Longest backticked span treated as an identifier.
const SPAN_MAX: usize = 120;

/// Splits `text` into pieces of at most [`CHUNK_BYTES`], at line breaks where possible and at
/// character boundaries inside overlong lines. Empty pieces are dropped.
pub fn chunks(text: &str) -> Vec<&str> {
    chunks_of(text, CHUNK_BYTES)
}

fn chunks_of(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let rest = &text[start..];
        if rest.len() <= max {
            push_trimmed(&mut out, rest);
            break;
        }
        let mut end = start + max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        // Prefer the last line break in the window, but not when that wastes most of it.
        let window = &text[start..end];
        let after = |at: usize| start + at + window[at..].chars().next().map_or(1, char::len_utf8);
        let cut = match window.rfind('\n') {
            Some(at) if at >= max / 2 => after(at),
            _ => match window.rfind(char::is_whitespace) {
                Some(at) if at >= max / 2 => after(at),
                _ => end,
            },
        };
        push_trimmed(&mut out, &text[start..cut]);
        start = cut;
    }
    out
}

fn push_trimmed<'a>(out: &mut Vec<&'a str>, piece: &'a str) {
    let piece = piece.trim();
    if !piece.is_empty() {
        out.push(piece);
    }
}

/// Identifier-like tokens of `text`, space separated: paths (`crates/daemon/src/lib.rs`), flags
/// (`--locked`, `-p`), Rust paths (`crate::paths`), symbols (`parse_line`, `FooBar`, `run()`),
/// and backticked spans. They get their own FTS column so a query for an identifier outranks
/// prose that merely mentions the same words.
pub fn idents(text: &str) -> String {
    let mut collector = Collector::default();

    // Backticked spans first: the author marked them as code.
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let span = after[..close].trim();
        if !span.is_empty() && span.len() <= SPAN_MAX && !span.contains('\n') {
            collector.add(span);
        }
        rest = &after[close + 1..];
    }

    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\''
                    | '`'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | ','
                    | ';'
                    | ':'
                    | '!'
                    | '?'
            )
        });
        let token = token.trim_end_matches(['.', ':']);
        if token.len() >= 2 && token.len() <= SPAN_MAX && is_identifier(token) {
            collector.add(token);
        }
    }
    collector.out
}

/// Space-joined, deduplicated, bounded list of identifiers.
#[derive(Default)]
struct Collector {
    out: String,
    seen: std::collections::HashSet<String>,
}

impl Collector {
    fn add(&mut self, token: &str) {
        if self.seen.len() >= IDENTS_MAX || self.out.len() + token.len() + 1 > IDENTS_BYTES {
            return;
        }
        if self.seen.insert(token.to_owned()) {
            if !self.out.is_empty() {
                self.out.push(' ');
            }
            self.out.push_str(token);
        }
    }
}

fn is_identifier(token: &str) -> bool {
    is_flag(token) || is_path(token) || token.contains("::") || is_symbol(token)
}

fn is_flag(token: &str) -> bool {
    let name = token.strip_prefix("--").or_else(|| token.strip_prefix('-'));
    let Some(name) = name else { return false };
    let name = name.split('=').next().unwrap_or(name);
    name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_path(token: &str) -> bool {
    token.contains('/')
        && token.chars().any(char::is_alphanumeric)
        && !token.starts_with("//")
        && !token.contains("://")
        && token.chars().all(|c| {
            c.is_alphanumeric()
                || matches!(
                    c,
                    '/' | '.' | '_' | '-' | '~' | '@' | '+' | '*' | '=' | ',' | '%' | '#'
                )
        })
}

fn is_symbol(token: &str) -> bool {
    let word = token.strip_suffix("()").unwrap_or(token);
    if word.is_empty()
        || !word
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        || !word.chars().next().is_some_and(char::is_alphabetic)
    {
        return false;
    }
    let has_underscore = word.contains('_');
    let camel =
        word.chars().skip(1).any(char::is_uppercase) && word.chars().any(char::is_lowercase);
    let call = token.ends_with("()");
    has_underscore || camel || call
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk_and_empty_text_none() {
        assert_eq!(chunks("hello"), ["hello"]);
        assert!(chunks("  \n ").is_empty());
        assert!(chunks("").is_empty());
    }

    #[test]
    fn long_text_splits_at_line_breaks_within_the_limit() {
        let line = "x".repeat(99);
        let text = (0..200)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let pieces = chunks(&text);
        assert!(pieces.len() >= 5, "{}", pieces.len());
        for piece in &pieces {
            assert!(piece.len() <= CHUNK_BYTES, "{}", piece.len());
            assert!(piece.lines().all(|l| l.len() == 99), "split mid-line");
        }
        let rejoined: usize = pieces.iter().map(|p| p.lines().count()).sum();
        assert_eq!(rejoined, 200);
    }

    #[test]
    fn overlong_lines_split_on_character_boundaries() {
        let text = "é".repeat(5000);
        let pieces = chunks(&text);
        assert_eq!(pieces.len(), 3);
        assert!(pieces.iter().all(|p| p.len() <= CHUNK_BYTES));
        assert_eq!(pieces.concat(), text);
    }

    #[test]
    fn multi_byte_whitespace_is_a_safe_cut() {
        let text = format!("{}\u{a0}{}", "x".repeat(3000), "y".repeat(3000));
        let pieces = chunks(&text);
        assert!(pieces.iter().all(|p| p.len() <= CHUNK_BYTES));
        assert_eq!(
            pieces.concat().replace(['\u{a0}'], ""),
            text.replace('\u{a0}', "")
        );
        let text = format!("{}\u{3000}{}", "あ".repeat(1500), "い".repeat(1500));
        assert!(chunks(&text).iter().all(|p| p.len() <= CHUNK_BYTES));
    }

    #[test]
    fn identifiers_are_extracted() {
        let text = "Ran `cargo test -p daemon` in crates/daemon/src/lib.rs with --locked; \
                    see crate::paths::Paths and parse_line(), FooBar plus http://x.y/z and plain words.";
        let found = idents(text);
        let tokens: Vec<_> = found.split(' ').collect();
        for expected in [
            "cargo",
            "-p",
            "crates/daemon/src/lib.rs",
            "--locked",
            "crate::paths::Paths",
            "parse_line",
            "FooBar",
        ] {
            assert!(
                found.contains(expected),
                "{expected} missing from {found:?} ({tokens:?})"
            );
        }
        assert!(found.contains("cargo test -p daemon"), "{found:?}");
        assert!(!tokens.contains(&"plain"), "{found:?}");
        assert!(!found.contains("http://x.y/z"), "{found:?}");
    }

    #[test]
    fn idents_are_bounded() {
        let text = (0..500)
            .map(|i| format!("some_symbol_{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let found = idents(&text);
        assert!(found.len() <= IDENTS_BYTES);
        assert!(found.split(' ').count() <= IDENTS_MAX);
    }
}
