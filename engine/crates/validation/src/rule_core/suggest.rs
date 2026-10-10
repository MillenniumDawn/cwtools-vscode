const MAX_DISTANCE: usize = 2;

const MIN_CANDIDATE_LEN: usize = 3;

#[derive(Default)]
struct DistanceScratch {
    key_chars: Vec<char>,
    candidate_chars: Vec<char>,
    prev: Vec<usize>,
    cur: Vec<usize>,
}

#[cfg(test)]
fn text_len(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.chars().count()
    }
}

fn bounded_distance_slices<T>(
    a: &[T],
    b: &[T],
    max: usize,
    prev: &mut Vec<usize>,
    cur: &mut Vec<usize>,
    equal: impl Fn(&T, &T) -> bool,
) -> Option<usize> {
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > max {
        return None;
    }

    prev.resize(m + 1, 0);
    cur.resize(m + 1, 0);
    for (j, value) in prev.iter_mut().enumerate() {
        *value = j;
    }
    for i in 1..=n {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=m {
            let cost = usize::from(!equal(&a[i - 1], &b[j - 1]));
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

#[cfg(test)]
fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let (n, m) = (text_len(a), text_len(b));
    // Reject a length mismatch before creating any character buffers or DP rows.
    if n.abs_diff(m) > max {
        return None;
    }

    let mut scratch = DistanceScratch::default();
    if a.is_ascii() && b.is_ascii() {
        return bounded_distance_slices(
            a.as_bytes(),
            b.as_bytes(),
            max,
            &mut scratch.prev,
            &mut scratch.cur,
            |x, y| x.eq_ignore_ascii_case(y),
        );
    }

    scratch
        .key_chars
        .extend(a.chars().map(|c| c.to_ascii_lowercase()));
    scratch
        .candidate_chars
        .extend(b.chars().map(|c| c.to_ascii_lowercase()));
    bounded_distance_slices(
        &scratch.key_chars,
        &scratch.candidate_chars,
        max,
        &mut scratch.prev,
        &mut scratch.cur,
        |x, y| x == y,
    )
}

pub(super) fn best_suggestion<'a, I>(key: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    best_suggestion_with_scratch(key, candidates, &mut DistanceScratch::default())
}

fn best_suggestion_with_scratch<'a, I>(
    key: &str,
    candidates: I,
    scratch: &mut DistanceScratch,
) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let key_ascii = key.is_ascii();
    let key_len = if key_ascii {
        key.len()
    } else {
        key.chars().count()
    };
    let mut key_chars_ready = false;
    let mut best: Option<(&'a str, usize)> = None;
    let mut tied = false;

    for cand in candidates {
        let cand_ascii = cand.is_ascii();
        let cand_len = if cand_ascii {
            cand.len()
        } else {
            cand.chars().count()
        };
        if cand_len < MIN_CANDIDATE_LEN || key_len.abs_diff(cand_len) > MAX_DISTANCE {
            continue;
        }

        let d = if key_ascii && cand_ascii {
            bounded_distance_slices(
                key.as_bytes(),
                cand.as_bytes(),
                MAX_DISTANCE,
                &mut scratch.prev,
                &mut scratch.cur,
                |x, y| x.eq_ignore_ascii_case(y),
            )
        } else {
            // The unknown key is the same for every candidate. Materialize it at most once,
            // and only after a candidate survives the allocation-free length filter.
            if !key_chars_ready {
                scratch.key_chars.clear();
                scratch
                    .key_chars
                    .extend(key.chars().map(|c| c.to_ascii_lowercase()));
                key_chars_ready = true;
            }
            scratch.candidate_chars.clear();
            scratch
                .candidate_chars
                .extend(cand.chars().map(|c| c.to_ascii_lowercase()));
            bounded_distance_slices(
                &scratch.key_chars,
                &scratch.candidate_chars,
                MAX_DISTANCE,
                &mut scratch.prev,
                &mut scratch.cur,
                |x, y| x == y,
            )
        };
        let Some(d) = d else {
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
    fn long_ascii_key_with_short_candidates_allocates_nothing_in_the_scan() {
        let key = "x".repeat(2 * 1024 * 1024);
        let candidates = ["name", "count", "required_field"];
        let result = Cell::new(Some("sentinel"));

        let allocations = count_allocations(|| {
            result.set(best_suggestion(&key, candidates));
        });
        assert_eq!(result.get(), None);
        println!("rejected_2MiB_ascii_key_allocations={allocations}");
        assert_eq!(
            allocations, 0,
            "length rejects should not allocate scan scratch"
        );
    }

    #[test]
    fn allocation_count_is_flat_from_one_to_twenty_thousand_candidates() {
        let small = ["counx"];
        let large = vec!["counx"; 20_000];
        let small_result = Cell::new(None);
        let large_result = Cell::new(None);

        let small_allocations = count_allocations(|| {
            small_result.set(best_suggestion("count", small));
        });
        let large_allocations = count_allocations(|| {
            large_result.set(best_suggestion("count", large.iter().copied()));
        });

        assert_eq!(small_result.get(), Some("counx"));
        assert_eq!(large_result.get(), Some("counx"));
        println!(
            "suggestion_scan_allocations candidates=1:{small_allocations} candidates=20000:{large_allocations}"
        );
        assert!(
            small_allocations > 0,
            "the counter must observe DP row allocation"
        );
        assert_eq!(large_allocations, small_allocations);
    }

    #[test]
    fn many_ascii_candidates_reuse_the_same_dp_rows() {
        let candidates = vec!["counx"; 20_000];
        let mut scratch = DistanceScratch::default();
        assert_eq!(
            best_suggestion_with_scratch("count", candidates.iter().copied(), &mut scratch),
            Some("counx")
        );
        let first_capacities = (scratch.prev.capacity(), scratch.cur.capacity());
        assert!(first_capacities.0 >= 6 && first_capacities.1 >= 6);

        let many_unexpected_keys = ["coubt", "counx", "coutn", "contx"];
        for key in many_unexpected_keys {
            let _ = best_suggestion_with_scratch(key, candidates.iter().copied(), &mut scratch);
            assert_eq!(
                (scratch.prev.capacity(), scratch.cur.capacity()),
                first_capacities
            );
        }
    }

    #[test]
    fn unicode_candidates_reuse_character_buffers_and_dp_rows() {
        let candidates = vec!["cöunt"; 20_000];
        let mut scratch = DistanceScratch::default();

        assert_eq!(
            best_suggestion_with_scratch("cönt", candidates.iter().copied(), &mut scratch),
            Some("cöunt")
        );
        let capacities = (
            scratch.key_chars.capacity(),
            scratch.candidate_chars.capacity(),
            scratch.prev.capacity(),
            scratch.cur.capacity(),
        );
        assert!(capacities.0 >= 4 && capacities.1 >= 5);
        assert!(capacities.2 >= 6 && capacities.3 >= 6);

        assert_eq!(
            best_suggestion_with_scratch("cönt", candidates.iter().copied(), &mut scratch),
            Some("cöunt")
        );
        assert_eq!(
            (
                scratch.key_chars.capacity(),
                scratch.candidate_chars.capacity(),
                scratch.prev.capacity(),
                scratch.cur.capacity(),
            ),
            capacities
        );
    }
}
