//! Unicode case folding and browser-compatible search offsets.
use crate::{select, text, work::Work};
use anyhow::Result;
use serde_json::{Value, json};
use unicode_casefold::UnicodeCaseFold;

pub(crate) fn fold(s: &str) -> String {
    s.case_fold().collect()
}

pub fn first_match(items: &[Value], query: &str, work: &Work) -> Result<Option<Value>> {
    Ok(matches(items, query, 1, work)?
        .first()
        .map(|m| select(m, &["key", "field", "term", "preview"])))
}

pub fn matches(items: &[Value], query: &str, limit: usize, work: &Work) -> Result<Vec<Value>> {
    let query = fold(query.trim());
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut matches = Vec::new();
    for item in items {
        work.check()?;
        for field in ["body", "subject"] {
            let value = text(item, field);
            let folded = fold(value);
            if !folded.contains(&query) {
                continue;
            }
            let characters: Vec<_> = value.chars().collect();
            let mut utf16 = vec![0];
            for c in &characters {
                utf16.push(utf16.last().unwrap() + c.len_utf16());
            }
            let mut positions = Vec::with_capacity(folded.len());
            for (index, c) in value.chars().enumerate() {
                for folded in c.case_fold() {
                    positions.extend(std::iter::repeat_n(index, folded.len_utf8()));
                }
            }
            let mut previous_end = 0;
            for (offset, _) in folded.match_indices(&query) {
                work.check()?;
                let start = positions[offset];
                let end = positions[offset + query.len() - 1] + 1;
                if start < previous_end {
                    continue;
                }
                previous_end = end;
                let left = start.saturating_sub(12);
                let count = characters.len();
                let right = (left + 140).max(end).min(count);
                let preview = format!(
                    "{}{}{}",
                    if left > 0 { "…" } else { "" },
                    characters[left..right].iter().collect::<String>(),
                    if right < count { "…" } else { "" }
                );
                matches.push(json!({"key":item["keys"][0],"field":field,"term":characters[start..end].iter().collect::<String>(),"preview":preview,
                "start":utf16[start],"end":utf16[end]}));
                if matches.len() == limit {
                    return Ok(matches);
                }
            }
        }
    }
    Ok(matches)
}
