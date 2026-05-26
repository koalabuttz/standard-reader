//! A tiny inverted index over `textContent`.
//!
//! `textContent` is the spec's purpose-built plaintext representation, so v1 search
//! is deliberately humble: tokenize, map term → doc IDs, intersect. Pure logic that
//! a `Store` impl can persist however it likes. When ranking/fuzzy/phrase queries
//! are wanted, this is the swap-point for `tantivy` (pure-Rust) without touching
//! the rest of the engine.

use std::collections::{BTreeSet, HashMap};

/// Lowercased alphanumeric tokens.
pub fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
}

/// In-memory term → document-id index. Document IDs are caller-assigned (a `Store`
/// maps them to AT-URIs).
#[derive(Debug, Default)]
pub struct Index {
    postings: HashMap<String, BTreeSet<u64>>,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, doc_id: u64, text: &str) {
        for term in tokenize(text) {
            self.postings.entry(term).or_default().insert(doc_id);
        }
    }

    /// Documents matching *all* query terms (AND semantics).
    pub fn query(&self, query: &str) -> BTreeSet<u64> {
        let mut terms = tokenize(query);
        let Some(first) = terms.next() else {
            return BTreeSet::new();
        };
        let mut hits = self.postings.get(&first).cloned().unwrap_or_default();
        for term in terms {
            match self.postings.get(&term) {
                Some(p) => hits.retain(|id| p.contains(id)),
                None => return BTreeSet::new(),
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn and_query_intersects() {
        let mut idx = Index::new();
        idx.insert(1, "bottom surgery is a big change");
        idx.insert(2, "a lifechanging surgery story");
        assert_eq!(idx.query("surgery").len(), 2);
        assert_eq!(idx.query("bottom surgery"), [1].into_iter().collect());
        assert!(idx.query("nonexistent").is_empty());
    }
}
