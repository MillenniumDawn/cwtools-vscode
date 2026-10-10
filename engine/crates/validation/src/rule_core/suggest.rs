const MAX_DISTANCE: usize = 2;

const MIN_CANDIDATE_LEN: usize = 3;

fn lowercase_into(text: &str, chars: &mut Vec<char>) {
    chars.clear();
    chars.extend(text.chars().map(|c| c.to_ascii_lowercase()));
}

/// `prev` and `cur` are scratch rows, reused across the candidates of one scan.
fn bounded_distance(
    a: &[char],
    b: &[char],
    max: usize,
    prev: &mut Vec<usize>,
    cur: &mut Vec<usize>,
) -> Option<usize> {
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > max {
        return None;
    }
    prev.clear();
    prev.extend(0..=m);
    cur.resize(m + 1, 0);
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
        std::mem::swap(prev, cur);
    }
    Some(prev[m]).filter(|&d| d <= max)
}

pub(super) fn best_suggestion<'a, I>(key: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let key_len = key.chars().count();
    let (mut key_chars, mut cand_chars) = (Vec::new(), Vec::new());
    let (mut prev, mut cur) = (Vec::new(), Vec::new());
    let mut best: Option<(&'a str, usize)> = None;
    let mut tied = false;
    for cand in candidates {
        let cand_len = cand.chars().count();
        if cand_len < MIN_CANDIDATE_LEN || key_len.abs_diff(cand_len) > MAX_DISTANCE {
            continue;
        }
        // Lowercase the key once, and only when a candidate passes the length filter.
        if key_chars.is_empty() {
            lowercase_into(key, &mut key_chars);
        }
        lowercase_into(cand, &mut cand_chars);
        let Some(d) = bounded_distance(&key_chars, &cand_chars, MAX_DISTANCE, &mut prev, &mut cur)
        else {
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
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    use super::*;

    struct CountingAllocator;

    #[global_allocator]
    static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

    thread_local! {
        static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    fn record_allocation() {
        if COUNT_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATION_COUNT.try_with(|count| count.set(count.get() + 1));
        }
    }

    // The thread-local gate confines allocation counts to the test's calling thread, so
    // concurrent tests and test-harness bookkeeping do not affect the measurement.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record_allocation();
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record_allocation();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            record_allocation();
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    struct StopCounting;

    impl Drop for StopCounting {
        fn drop(&mut self) {
            COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        }
    }

    fn count_allocations(f: impl FnOnce()) -> usize {
        ALLOCATION_COUNT.with(|count| count.set(0));
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
        let stop = StopCounting;
        f();
        drop(stop);
        ALLOCATION_COUNT.with(Cell::get)
    }

    fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
        let (mut a_chars, mut b_chars) = (Vec::new(), Vec::new());
        lowercase_into(a, &mut a_chars);
        lowercase_into(b, &mut b_chars);
        super::bounded_distance(&a_chars, &b_chars, max, &mut Vec::new(), &mut Vec::new())
    }

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

    #[test]
    fn long_key_with_short_candidates_allocates_nothing() {
        let key = "x".repeat(2 * 1024 * 1024);
        let candidates = ["name", "count", "required_field"];
        let result = Cell::new(Some("sentinel"));

        let allocations = count_allocations(|| {
            result.set(best_suggestion(&key, candidates));
        });
        assert_eq!(result.get(), None);
        assert_eq!(allocations, 0, "length rejects should not allocate");
    }

    #[test]
    fn allocation_count_is_flat_from_one_to_twenty_thousand_candidates() {
        for (key, cand) in [("count", "counx"), ("cönt", "cöunt")] {
            let large = vec![cand; 20_000];
            let small_result = Cell::new(None);
            let large_result = Cell::new(None);

            let small_allocations = count_allocations(|| {
                small_result.set(best_suggestion(key, [cand]));
            });
            let large_allocations = count_allocations(|| {
                large_result.set(best_suggestion(key, large.iter().copied()));
            });

            assert_eq!(small_result.get(), Some(cand));
            assert_eq!(large_result.get(), Some(cand));
            assert!(
                small_allocations > 0,
                "the counter must observe the scratch buffers"
            );
            assert_eq!(large_allocations, small_allocations, "{key}");
        }
    }
}
