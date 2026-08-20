//! Did-you-mean suggestions for unknown keys (CW262/CW263). A bounded, case-
//! insensitive edit-distance scan over the sibling rule keys picks a single close
//! match, which the emit site attaches as a [`SuggestedFix`] title/edit. This runs
//! ONLY on the error path (an unexpected key about to be reported), never when a
//! key matches — so the DP cost is paid only for keys already known to be wrong.
//!
//! The suggestion is pure fix metadata: it never changes the diagnostic message,
//! code, or position, so it is corpus-inert (proven by the CLI `fix_payload_is_
//! inert_in_report` guard).

/// Max edit distance for a suggestion. A distance of 2 covers a transposition or
/// two typos; beyond that the "did you mean" is more noise than help.
const MAX_DISTANCE: usize = 2;

/// A candidate rule key shorter than this is skipped: a 1-2 char key (`x`, `id`)
/// is within distance 2 of almost anything, producing nonsense suggestions.
const MIN_CANDIDATE_LEN: usize = 3;

/// Case-insensitive (ASCII) Levenshtein distance between `a` and `b`, bounded by
/// `max`: returns `None` the moment the distance is known to exceed `max`, so no
/// full matrix is computed for far-apart strings. Classic two-row DP; the length
/// difference alone short-circuits when it already exceeds `max`, and each row is
/// abandoned once its running minimum passes `max` (distances only grow downward).
pub(super) fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().map(|c| c.to_ascii_lowercase()).collect();
    let b: Vec<char> = b.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (n, m) = (a.len(), b.len());
    // A length gap larger than `max` cannot be bridged by <= `max` edits.
    if n.abs_diff(m) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        // Every cell below and to the right can only be >= this row's minimum, so
        // once the whole row exceeds `max` the final distance does too.
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    Some(prev[m]).filter(|&d| d <= max)
}

/// The single closest candidate to `key`, or `None` when there is no candidate
/// within [`MAX_DISTANCE`] or two candidates tie for the minimal distance. Runs on
/// the error path only. Candidates shorter than [`MIN_CANDIDATE_LEN`] are ignored.
///
/// A distance-0 match (case-only difference) is a valid suggestion in principle,
/// but cannot occur at the CW262/CW263 sites: the engine matches keys case-
/// insensitively, so a case-only match would already have satisfied a rule and no
/// unexpected-key error would fire. The minimum distance reached here is 1.
pub(super) fn best_suggestion<'a, I>(key: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut best: Option<(&'a str, usize)> = None;
    let mut tied = false;
    for cand in candidates {
        if cand.chars().count() < MIN_CANDIDATE_LEN {
            continue;
        }
        let Some(d) = bounded_distance(key, cand, MAX_DISTANCE) else {
            continue;
        };
        match best {
            Some((_, bd)) if d < bd => {
                best = Some((cand, d));
                tied = false;
            }
            // A second candidate at the same minimal distance is a tie (unless it
            // is the same key spelled the same way, e.g. an overload duplicate).
            Some((bstr, bd)) if d == bd && !cand.eq_ignore_ascii_case(bstr) => {
                tied = true;
            }
            Some(_) => {}
            None => best = Some((cand, d)),
        }
    }
    match best {
        Some((cand, _)) if !tied => Some(cand),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_basic_edits() {
        assert_eq!(bounded_distance("name", "name", 2), Some(0));
        assert_eq!(bounded_distance("naem", "name", 2), Some(2)); // transposition
        assert_eq!(bounded_distance("cont", "count", 2), Some(1)); // one deletion
        assert_eq!(bounded_distance("namee", "name", 2), Some(1)); // one insertion
    }

    #[test]
    fn distance_is_case_insensitive() {
        assert_eq!(bounded_distance("NAME", "name", 2), Some(0));
        assert_eq!(bounded_distance("Naem", "name", 2), Some(2));
    }

    #[test]
    fn distance_beyond_threshold_is_none() {
        assert_eq!(bounded_distance("xyzzy", "name", 2), None);
        assert_eq!(bounded_distance("count", "required_field", 2), None);
    }

    #[test]
    fn length_gap_shortcuts_to_none() {
        // |len(a) - len(b)| = 3 > 2: no full matrix, immediate None even though the
        // shared prefix is long.
        assert_eq!(bounded_distance("count", "co", 2), None);
        assert_eq!(bounded_distance("ab", "abcde", 2), None);
    }

    #[test]
    fn best_suggestion_unique_close_match() {
        let cands = ["name", "count", "required_field"];
        assert_eq!(best_suggestion("cont", cands), Some("count"));
        assert_eq!(best_suggestion("naem", cands), Some("name"));
    }

    #[test]
    fn best_suggestion_no_close_match_is_none() {
        let cands = ["name", "count", "required_field"];
        assert_eq!(best_suggestion("xyzzy", cands), None);
    }

    #[test]
    fn best_suggestion_tie_is_none() {
        // "rat" is distance 1 from both "cat" and "bat": ambiguous, so no fix.
        let cands = ["cat", "bat"];
        assert_eq!(best_suggestion("rat", cands), None);
    }

    #[test]
    fn best_suggestion_skips_short_candidates() {
        // "ba" -> "ab" is distance 2 but the candidate is only 2 chars: skipped.
        let cands = ["ab"];
        assert_eq!(best_suggestion("ba", cands), None);
    }

    #[test]
    fn best_suggestion_prefers_strictly_closer() {
        // "coun" is distance 1 from "count", 2 from "county": the closer wins even
        // though a farther candidate also lands within threshold.
        let cands = ["count", "county"];
        assert_eq!(best_suggestion("coun", cands), Some("count"));
    }

    #[test]
    fn best_suggestion_duplicate_key_is_not_a_tie() {
        // The same key can appear twice (rule overloads); that is not ambiguity.
        let cands = ["count", "count"];
        assert_eq!(best_suggestion("cont", cands), Some("count"));
    }
}
