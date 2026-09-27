//! Source framing helpers used at PET request and writeback boundaries.
//!
//! These functions locate byte ranges but never decide Rocq semantics; PET
//! remains authoritative for parsing and proof validity.

use crate::{CanonicalTactic, Error, ErrorKind};
use std::ops::Range;

/// Canonicalize one complete tactic sentence for storage and PET replay.
///
/// Unlike declaration normalization, the terminal dot is semantic input to
/// PET and must be retained. PET's AST, not a local keyword list, determines
/// whether this sentence is a proof command before replay executes it.
pub(crate) fn canonical_tactic(source: &str) -> crate::Result<CanonicalTactic> {
    // Design note: PET must see the caller's exact sentence. Normalizing
    // whitespace changes Rocq string literals and can change tactic meaning.
    let normalized = source.trim();
    if normalized.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "tactic must be non-empty",
        ));
    }
    Ok(CanonicalTactic(normalized.to_owned()))
}

/// Returns sentence byte ranges.  Comments are nested and strings use Rocq's
/// doubled-quote escape.  Dots between two identifier/digit characters are not
/// sentence terminators, preserving qualified names and decimal notation.
pub(crate) fn sentence_ranges(source: &str) -> crate::Result<Vec<Range<usize>>> {
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
            // Design note: tuple projections such as `(f x).1` are part of a
            // term even though the character before the dot is `)`. PET's AST
            // validates every framed command before writeback or trust use.
            let qualified = following.is_some_and(|x| x.is_ascii_digit())
                || (previous.is_some_and(|x| x.is_alphanumeric() || x == '_')
                    && following.is_some_and(|x| x.is_alphanumeric() || x == '_'));
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
    // Design note: an unterminated final fragment is still a byte range. PET
    // owns the parse error; this scanner must not duplicate Rocq syntax.
    if significant || ((comment_depth != 0 || string) && !source[start..].trim().is_empty()) {
        ranges.push(start..source.len());
    }
    Ok(ranges)
}
