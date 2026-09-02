use std::collections::{BTreeSet, HashSet};

use super::subsequence_match;

type Ranked<'a> = (bool, &'a str);

fn rank<'a>(key: &'a str, token: &str) -> Ranked<'a> {
    let starts = key
        .get(..token.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(token));
    (!starts, key)
}

struct TopK<'a> {
    selected: BTreeSet<Ranked<'a>>,
    cap: usize,
}

impl<'a> TopK<'a> {
    fn new(cap: usize) -> Self {
        Self {
            selected: BTreeSet::new(),
            cap,
        }
    }

    fn len(&self) -> usize {
        self.selected.len()
    }

    fn push(&mut self, ranked: Ranked<'a>) {
        if self.selected.len() < self.cap {
            self.selected.insert(ranked);
        } else if self
            .selected
            .last()
            .is_some_and(|largest| ranked < *largest)
            && self.selected.insert(ranked)
        {
            self.selected.pop_last();
        }
    }

    fn into_keys(self) -> HashSet<String> {
        self.selected
            .into_iter()
            .map(|(_, key)| key.to_owned())
            .collect()
    }
}

pub(crate) fn select_loc_keys<'a>(
    keys: impl Iterator<Item = &'a str>,
    token: &str,
    cap: usize,
) -> HashSet<String> {
    let mut top = TopK::new(cap);
    for key in keys.filter(|key| subsequence_match(key, token)) {
        top.push(rank(key, token));
    }
    top.into_keys()
}

fn char_mask(text: &str, non_ascii: u64) -> u64 {
    if !text.is_ascii() {
        return non_ascii;
    }
    text.bytes()
        .fold(0u64, |mask, b| mask | 1 << (b.to_ascii_lowercase() & 63))
}

pub(crate) struct LocKeyIndex {
    blob: String,
    offsets: Vec<usize>,
    masks: Vec<u64>,
}

impl LocKeyIndex {
    pub(crate) fn build<'a>(keys: impl Iterator<Item = &'a str>) -> Self {
        let mut sorted: Vec<&str> = keys.collect();
        sorted.sort_unstable();
        sorted.dedup();
        let mut blob = String::with_capacity(sorted.iter().map(|key| key.len()).sum());
        let mut offsets = Vec::with_capacity(sorted.len() + 1);
        let mut masks = Vec::with_capacity(sorted.len());
        for key in sorted {
            offsets.push(blob.len());
            masks.push(char_mask(key, !0));
            blob.push_str(key);
        }
        offsets.push(blob.len());
        Self {
            blob,
            offsets,
            masks,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    fn key(&self, i: usize) -> &str {
        &self.blob[self.offsets[i]..self.offsets[i + 1]]
    }

    fn prefix_start(&self, prefix: &str) -> usize {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.key(mid) < prefix {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub(crate) fn select<'a>(
        &'a self,
        token: &str,
        overlay: impl Iterator<Item = &'a str>,
        cap: usize,
    ) -> HashSet<String> {
        let mut top = TopK::new(cap);
        let prefix = token.to_ascii_lowercase();
        for i in self.prefix_start(&prefix)..self.len() {
            let key = self.key(i);
            if top.len() == cap || !key.starts_with(prefix.as_str()) {
                break;
            }
            top.push(rank(key, token));
        }
        if top.len() < cap {
            let needle = char_mask(token, 0);
            for (i, mask) in self.masks.iter().enumerate() {
                if mask & needle != needle {
                    continue;
                }
                let key = self.key(i);
                if subsequence_match(key, token) {
                    top.push(rank(key, token));
                    if top.len() == cap {
                        break;
                    }
                }
            }
        }
        for key in overlay.filter(|key| subsequence_match(key, token)) {
            top.push(rank(key, token));
        }
        top.into_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Vec<String> {
        let stems = [
            "focus", "idea", "decision", "event", "state", "unit", "tech", "party",
        ];
        let owners = ["mds", "usa", "ger", "sov", "generic", "zzz"];
        let mut keys = Vec::new();
        for (i, owner) in owners.iter().enumerate() {
            for (j, stem) in stems.iter().enumerate() {
                for n in 0..40 {
                    keys.push(format!("{}_{}_{:03}", owner, stem, n * (i + j + 1) % 97));
                    keys.push(format!("{}_{}_{:03}_desc", owner, stem, n));
                }
            }
        }
        keys.sort();
        keys.dedup();
        keys
    }

    fn tokens() -> Vec<&'static str> {
        vec![
            "",
            "f",
            "z",
            "m",
            "mds",
            "focus",
            "mds_focus",
            "_desc",
            "fcs",
            "MDS",
            "Focus",
            "qqq",
            "mds_focus_000",
            "0",
            "zzz_party",
        ]
    }

    #[test]
    fn indexed_select_matches_linear_sweep() {
        let keys = corpus();
        let index = LocKeyIndex::build(keys.iter().map(String::as_str));
        let overlay: Vec<String> = vec![
            "overlay_focus_key".to_string(),
            "mds_focus_001".to_string(),
            "aaa_first_key".to_string(),
        ];
        for cap in [0, 1, 7, 50, 1000, usize::MAX / 2] {
            for token in tokens() {
                let linear = select_loc_keys(
                    keys.iter()
                        .map(String::as_str)
                        .chain(overlay.iter().map(String::as_str)),
                    token,
                    cap,
                );
                let indexed = index.select(token, overlay.iter().map(String::as_str), cap);
                assert_eq!(
                    linear,
                    indexed,
                    "token {:?} cap {} diverged ({} vs {} keys)",
                    token,
                    cap,
                    linear.len(),
                    indexed.len()
                );
            }
        }
    }

    #[test]
    fn start_matches_win_the_cap_over_subsequence_matches() {
        let keys: Vec<String> = vec![
            "aaa_f_late".to_string(), // subsequence match on "f" only
            "fzz_first".to_string(),  // start-match
            "faa_second".to_string(), // start-match
        ];
        let index = LocKeyIndex::build(keys.iter().map(String::as_str));
        let picked = index.select("f", std::iter::empty(), 2);
        assert_eq!(
            picked,
            ["faa_second".to_string(), "fzz_first".to_string()]
                .into_iter()
                .collect::<HashSet<_>>()
        );
    }

    #[test]
    fn empty_index_and_empty_token_are_safe() {
        let index = LocKeyIndex::build(std::iter::empty());
        assert_eq!(index.len(), 0);
        assert!(index.select("", std::iter::empty(), 10).is_empty());
        assert!(index.select("abc", std::iter::empty(), 10).is_empty());

        let keys = ["b_key".to_string(), "a_key".to_string()];
        let index = LocKeyIndex::build(keys.iter().map(String::as_str));
        assert_eq!(index.select("", std::iter::empty(), 10).len(), 2);
    }

    #[test]
    fn overlay_keys_join_the_result() {
        let keys = ["scanned_focus".to_string()];
        let index = LocKeyIndex::build(keys.iter().map(String::as_str));
        let overlay = ["unsaved_focus".to_string()];
        let picked = index.select("focus", overlay.iter().map(String::as_str), 10);
        assert!(picked.contains("scanned_focus"));
        assert!(picked.contains("unsaved_focus"));
    }

    #[test]
    fn build_dedups_and_sorts() {
        let keys = [
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
        ];
        let index = LocKeyIndex::build(keys.iter().map(String::as_str));
        assert_eq!(index.len(), 3);
        let stored: Vec<&str> = (0..index.len()).map(|i| index.key(i)).collect();
        assert_eq!(stored, ["a", "b", "c"]);
    }
}
