const MAX_DISTANCE: usize = 2;

const MIN_CANDIDATE_LEN: usize = 3;

pub(super) fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().map(|c| c.to_ascii_lowercase()).collect();
    let b: Vec<char> = b.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (n, m) = (a.len(), b.len());
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
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    Some(prev[m]).filter(|&d| d <= max)
}

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
        let cands = ["cat", "bat"];
        assert_eq!(best_suggestion("rat", cands), None);
    }

    #[test]
    fn best_suggestion_skips_short_candidates() {
        let cands = ["ab"];
        assert_eq!(best_suggestion("ba", cands), None);
    }

    #[test]
    fn best_suggestion_prefers_strictly_closer() {
        let cands = ["count", "county"];
        assert_eq!(best_suggestion("coun", cands), Some("count"));
    }

    #[test]
    fn best_suggestion_duplicate_key_is_not_a_tie() {
        let cands = ["count", "count"];
        assert_eq!(best_suggestion("cont", cands), Some("count"));
    }
}
