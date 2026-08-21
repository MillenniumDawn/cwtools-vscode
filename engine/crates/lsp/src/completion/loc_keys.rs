//! Prefix-searchable view of the loc-key union, for loc completion.
//!
//! The union is the whole project's localisation namespace — ~400K keys on
//! Millennium Dawn with vanilla merged in. Completion only ever returns the
//! [`CONTEXT_CAP`](super::CONTEXT_CAP) best-ranked matches for the typed token,
//! but the request used to reach them by sweeping every key in the union on
//! every keystroke. [`LocKeyIndex`] is built once per workspace scan (alongside
//! the union it mirrors) and answers the same question by binary-searching a
//! sorted key blob, so the common case touches a few thousand keys instead of
//! four hundred thousand.
//!
//! The selection order is unchanged: start-matches ahead of looser subsequence
//! matches, then lexicographic, capped. [`select_loc_keys`] keeps the linear
//! sweep for the window before the first scan has built an index.

use std::collections::{BTreeSet, HashSet};

use super::subsequence_match;

/// One candidate ranked the way the loc list orders keys: a start-match sorts
/// ahead of a mere subsequence match, then lexicographically, so a later
/// truncation keeps the keys the user most likely meant.
type Ranked<'a> = (bool, &'a str);

fn rank<'a>(key: &'a str, token: &str) -> Ranked<'a> {
    let starts = key
        .get(..token.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(token));
    (!starts, key)
}

/// Bounded "keep the `cap` smallest" accumulator. Holds at most `cap` entries,
/// so a 400K-key sweep never materialises more than the response needs.
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

/// Linear sweep over `keys`, keeping the `cap` best-ranked subsequence matches
/// for `token`. Used before the first scan has built a [`LocKeyIndex`].
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

/// Which ASCII characters a string contains, folded into 64 buckets. A
/// subsequence match needs every needle character present in the haystack, so
/// `key & needle != needle` rejects a key without touching its text. Bucket
/// collisions (`'1'` lands on `'q'`) only ever let a key through, never reject
/// one. A non-ASCII key answers `!0` (always considered) and a non-ASCII needle
/// answers `0` (never rejects), because `to_lowercase` can fold a non-ASCII
/// character down to an ASCII one and the byte view would miss it.
fn char_mask(text: &str, non_ascii: u64) -> u64 {
    if !text.is_ascii() {
        return non_ascii;
    }
    text.bytes()
        .fold(0u64, |mask, b| mask | 1 << (b.to_ascii_lowercase() & 63))
}

/// Sorted loc keys in one contiguous blob plus their start offsets. Sorted
/// storage is what makes the start-match lookup a binary search; the blob (over
/// a `Vec<String>`) keeps the fallback sweep to one linear pass through memory
/// instead of 400K pointer chases, and costs one allocation.
pub(crate) struct LocKeyIndex {
    blob: String,
    /// `len() + 1` entries: each key's start, plus a trailing `blob.len()`.
    offsets: Vec<usize>,
    /// Per-key [`char_mask`], so the fallback sweep rejects most keys with one
    /// load and one `and` instead of walking their text.
    masks: Vec<u64>,
}

impl LocKeyIndex {
    /// Build from the loc-key union. Keys are stored as given, which relies on
    /// the same lowercased-key contract `LocIndex::exists_any` already has:
    /// every key in the union and in the live overlay is `to_lowercase`d at
    /// build, so byte order IS the case-insensitive order the start-match
    /// binary search needs. `select_loc_keys` stays exact for any input and is
    /// what runs before an index exists.
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

    /// Index of the first key at or after `prefix` in sort order.
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

    /// The `cap` best-ranked keys matching `token`, drawn from the index plus
    /// `overlay` (the live keys of open `.yml` files, too small and too churny
    /// to index). Identical result to [`select_loc_keys`] over the same keys.
    pub(crate) fn select<'a>(
        &'a self,
        token: &str,
        overlay: impl Iterator<Item = &'a str>,
        cap: usize,
    ) -> HashSet<String> {
        let mut top = TopK::new(cap);
        // Start-matches outrank everything else and are already in
        // lexicographic order, so the first `cap` of them are exactly the ones
        // that can survive the cap — the rest of the bucket never needs looking
        // at. A start-match is always a subsequence match too.
        let prefix = token.to_ascii_lowercase();
        for i in self.prefix_start(&prefix)..self.len() {
            let key = self.key(i);
            if top.len() == cap || !key.starts_with(prefix.as_str()) {
                break;
            }
            top.push(rank(key, token));
        }
        // Looser subsequence matches only fill what the start-matches left over.
        // This sweep is in lexicographic order too, and every start-match is
        // already in, so once the cap is full every later key ranks worse than
        // the current worst and the sweep can stop.
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
