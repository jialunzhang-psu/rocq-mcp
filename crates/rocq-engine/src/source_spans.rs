//! Rocq source boundary scanning only; semantic proof checking belongs to PET.

use super::{CanonicalTactic, Error, ErrorKind, Result};
use std::ops::Range;

pub(crate) fn normalize_fragment(source: &str) -> Result<String> {
    let stripped = strip_comments(source)?;
    Ok(stripped.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(crate) fn normalize_sentence(source: &str) -> String {
    normalize_fragment(source)
        .unwrap_or_default()
        .trim_end_matches('.')
        .trim()
        .to_owned()
}

/// Canonicalize one complete tactic sentence for storage and PET replay.
///
/// Unlike declaration normalization, the terminal dot is semantic input to
/// PET and must be retained. Proof terminators and declaration vernacular are
/// rejected before PET so a trace cannot escape its open proof.
pub(crate) fn canonical_tactic(source: &str) -> Result<CanonicalTactic> {
    let normalized = normalize_fragment(source)?;
    let normalized = normalized.trim();
    if normalized.is_empty() || !normalized.ends_with('.') {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "tactic must be one complete sentence",
        ));
    }
    let first = lexical_words(normalized)
        .into_iter()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        first.as_str(),
        "admit"
            | "admitted"
            | "abort"
            | "qed"
            | "defined"
            | "save"
            | "restart"
            | "undo"
            | "back"
            | "reset"
            | "drop"
            | "show"
            | "guarded"
            | "proof"
            | "theorem"
            | "lemma"
            | "fact"
            | "remark"
            | "corollary"
            | "proposition"
            | "definition"
            | "fixpoint"
            | "cofixpoint"
            | "axiom"
            | "parameter"
            | "parameters"
            | "variable"
            | "variables"
            | "module"
            | "section"
            | "end"
            | "require"
            | "import"
            | "export"
    ) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "proof-escaping vernacular is not a tactic",
        ));
    }
    Ok(CanonicalTactic(normalized.to_owned()))
}

pub(crate) fn lexical_words(sentence: &str) -> Vec<String> {
    let Ok(clean) = strip_comments(sentence) else {
        return Vec::new();
    };
    let chars = clean.chars().collect::<Vec<_>>();
    let mut words = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        while index < chars.len()
            && (chars[index].is_whitespace()
                || matches!(
                    chars[index],
                    ':' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
                ))
        {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        let start = index;
        while index < chars.len()
            && !chars[index].is_whitespace()
            && !matches!(
                chars[index],
                ':' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
            )
        {
            index += 1;
        }
        let word = chars[start..index].iter().collect::<String>();
        let word = word.trim_end_matches('.').to_owned();
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// Returns sentence byte ranges.  Comments are nested and strings use Rocq's
/// doubled-quote escape.  Dots between two identifier/digit characters are not
/// sentence terminators, preserving qualified names and decimal notation.
pub(crate) fn sentence_ranges(source: &str) -> Result<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut comment_depth = 0usize;
    let mut string = false;
    let mut significant = false;
    let chars = source.char_indices().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let (_byte, c) = chars[index];
        let next = chars.get(index + 1).copied();
        if comment_depth > 0 {
            if c == '(' && next.is_some_and(|(_, value)| value == '*') {
                comment_depth += 1;
                index += 2;
                continue;
            }
            if c == '*' && next.is_some_and(|(_, value)| value == ')') {
                comment_depth -= 1;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if string {
            if c == '"' {
                if next.is_some_and(|(_, value)| value == '"') {
                    index += 2;
                    continue;
                }
                string = false;
            }
            index += 1;
            continue;
        }
        if c == '(' && next.is_some_and(|(_, value)| value == '*') {
            comment_depth = 1;
            index += 2;
            continue;
        }
        if c == '"' {
            string = true;
            significant = true;
            index += 1;
            continue;
        }
        if !c.is_whitespace() {
            significant = true;
        }
        if c == '.' {
            let previous = index.checked_sub(1).and_then(|i| chars.get(i).map(|x| x.1));
            let following = next.map(|x| x.1);
            let qualified = previous.is_some_and(|x| x.is_alphanumeric() || x == '_')
                && following.is_some_and(|x| x.is_alphanumeric() || x == '_');
            if !qualified {
                let end = next.map_or(source.len(), |(position, _)| position);
                if significant {
                    ranges.push(start..end);
                }
                start = end;
                significant = false;
                index += 1;
                continue;
            }
        }
        index += 1;
    }
    if comment_depth != 0 || string {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "unterminated Rocq comment or string",
        ));
    }
    if significant {
        ranges.push(start..source.len());
    }
    Ok(ranges)
}

pub(crate) fn strip_comments(source: &str) -> Result<String> {
    let mut out = String::with_capacity(source.len());
    let chars = source.char_indices().collect::<Vec<_>>();
    let mut comment_depth = 0usize;
    let mut string = false;
    let mut index = 0;
    while index < chars.len() {
        let (_, c) = chars[index];
        let next = chars.get(index + 1).copied();
        if comment_depth > 0 {
            if c == '(' && next.is_some_and(|(_, value)| value == '*') {
                comment_depth += 1;
                out.push(' ');
                index += 2;
                continue;
            }
            if c == '*' && next.is_some_and(|(_, value)| value == ')') {
                comment_depth -= 1;
                out.push(' ');
                index += 2;
                continue;
            }
            if c == '\n' {
                out.push('\n');
            }
            index += 1;
            continue;
        }
        if string {
            out.push(c);
            if c == '"' {
                if next.is_some_and(|(_, value)| value == '"') {
                    out.push('"');
                    index += 2;
                    continue;
                }
                string = false;
            }
            index += 1;
            continue;
        }
        if c == '(' && next.is_some_and(|(_, value)| value == '*') {
            comment_depth = 1;
            out.push(' ');
            index += 2;
            continue;
        }
        if c == '"' {
            string = true;
        }
        out.push(c);
        index += 1;
    }
    if comment_depth != 0 || string {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "unterminated Rocq comment or string",
        ));
    }
    Ok(out)
}
