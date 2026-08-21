use parking_lot::{RwLock, RwLockReadGuard};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::Hasher;
use std::sync::Arc;

/// Number of independently locked shards. 64 is ~2.5x the core count of the
/// machines this runs on, so the parse threads collide rarely, and it is where a
/// sharded prototype stopped gaining on this corpus.
const SHARD_BITS: u32 = 6;
const SHARD_COUNT: usize = 1 << SHARD_BITS;
/// Bits left in a `StringId` for the shard-local slot index.
const SLOT_BITS: u32 = u32::BITS - SHARD_BITS;
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;

/// A unique identifier for an interned string.
///
/// The top `SHARD_BITS` bits name the owning shard and the rest is that shard's
/// slot index, so ids are *not* globally consecutive and their numeric order is
/// meaningless — compare them for equality only, and never persist one.
/// `u32::MAX` is reserved and never assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct StringId(pub u32);

/// Mirrors the F# `StringTokens` struct.
/// `lower`  → ID of the lower‑cased canonical form.
/// `normal` → ID of the exact (case‑preserving) string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringTokens {
    pub lower: StringId,
    pub normal: StringId,
}

/// Slot 0 of shard 0, reserved for the empty string.
const EMPTY_TOKENS: StringTokens = StringTokens {
    lower: StringId(0),
    normal: StringId(0),
};

#[inline]
fn split_id(id: StringId) -> (usize, usize) {
    ((id.0 >> SLOT_BITS) as usize, (id.0 & SLOT_MASK) as usize)
}

/// Pick the shard that owns `s`. Keyed on the *case-folded* bytes, so every
/// casing of a string lands in one shard and `lower` stays a single canonical id
/// no matter which thread interns which spelling first.
#[inline]
fn shard_of(s: &str) -> usize {
    let mut h = FxHasher::default();
    // Both arms must feed the hasher one byte at a time in the same way: the
    // Kelvin sign `\u{212A}` takes the non-ASCII arm but folds to plain `"k"`,
    // so the two arms do have to agree on the same byte stream.
    if s.is_ascii() {
        for &b in s.as_bytes() {
            h.write_u8(b.to_ascii_lowercase());
        }
    } else {
        for &b in s.to_lowercase().as_bytes() {
            h.write_u8(b);
        }
    }
    (h.finish() >> (u64::BITS - SHARD_BITS)) as usize
}

/// One independently locked slice of the interner.
///
/// Aligned to a cache line so that neighbouring shards' lock words never share
/// one; without it the parse threads keep invalidating each other's line and the
/// sharding buys much less than it should.
#[repr(align(64))]
struct Shard {
    /// Lower‑cased key → the canonical lower token (`lower == normal`).
    /// `Arc<str>` key shares the allocation with `id_to_string[lower slot]`.
    lower_map: FxHashMap<Arc<str>, StringTokens>,
    /// Exact (case‑preserving) key → the normal token that points to a lower ID.
    /// `Arc<str>` key shares the allocation with `id_to_string[normal slot]`.
    exact_map: FxHashMap<Arc<str>, StringTokens>,
    /// Dense per-shard array: slot → original or lower‑cased text.
    /// Slot 0 is the empty string in every shard, which keeps `StringId(0)`
    /// (shard 0, slot 0) resolving to `""`.
    id_to_string: Vec<Arc<str>>,
}

impl Shard {
    fn new(empty: &Arc<str>) -> Self {
        Self {
            lower_map: FxHashMap::default(),
            exact_map: FxHashMap::default(),
            id_to_string: vec![Arc::clone(empty)],
        }
    }

    /// Append `text` to this shard and hand back its id.
    fn push(&mut self, shard: usize, text: &Arc<str>) -> StringId {
        let slot = self.id_to_string.len() as u32;
        debug_assert!(
            slot < SLOT_MASK,
            "StringTable shard id space exhausted (u32::MAX is reserved)"
        );
        self.id_to_string.push(Arc::clone(text));
        StringId(((shard as u32) << SLOT_BITS) | slot)
    }
}

/// Thread‑safe string interner that preserves the F# `StringResourceManager`
/// semantics:
///
/// * Case‑insensitive lookup by lower‑cased key.
/// * Two IDs per logical entry: a *normal* ID (exact text) and a *lower* ID
///   (canonical lower‑cased form).  Multiple normal strings may share the same
///   lower ID.
pub struct StringTable {
    // Sharded by case-folded content: a process-wide write lock made interning
    // anti-scale (more parse threads = slower). Each shard is an RwLock because
    // validation is read-only on the table (only `get_string`), so once parsing
    // has interned everything the validation threads read concurrently.
    shards: Arc<[RwLock<Shard>; SHARD_COUNT]>,
}

impl Clone for StringTable {
    /// NOTE: this is an *aliasing* clone, not a deep copy. The clone shares the
    /// same underlying shards as the original, so a string interned through one
    /// handle is visible through the other. This is intentional (see the
    /// `shared_table` test). Cloning a `StringTable` hands out another handle
    /// to the same instance, not a process-wide singleton.
    fn clone(&self) -> Self {
        Self {
            shards: Arc::clone(&self.shards),
        }
    }
}

impl Default for StringTable {
    fn default() -> Self {
        Self::new()
    }
}

impl StringTable {
    pub fn new() -> Self {
        let empty: Arc<str> = Arc::from("");
        Self {
            shards: Arc::new(std::array::from_fn(|_| RwLock::new(Shard::new(&empty)))),
        }
    }

    /// Intern a string and return its `StringTokens`.
    ///
    /// * If the exact text has already been interned, the existing token is
    ///   returned (fast path via `exact_map`).
    /// * If the lower‑cased form exists but this exact text has never been
    ///   interned, a new `normal` ID is allocated that shares the existing `lower` ID.
    /// * If the lower‑cased form has never been seen, two consecutive slots of
    ///   the owning shard are allocated: `normal` (exact text) and `lower`.
    pub fn intern(&self, s: &str) -> StringTokens {
        // Reserved slot-0: the empty string maps to id 0 without consuming a
        // fresh id. All other strings start from slot 1 in their shard.
        if s.is_empty() {
            return EMPTY_TOKENS;
        }

        let idx = shard_of(s);
        let shard = &self.shards[idx];

        // Fast path: exact string already interned. This is the overwhelming
        // common case while parsing many files (identifiers repeat constantly),
        // and it takes a shared read lock on one shard only.
        {
            let guard = shard.read();
            if let Some(&existing) = guard.exact_map.get(s) {
                return existing;
            }
        }

        let mut guard = shard.write();
        intern_locked(&mut guard, idx, s)
    }

    /// Intern many strings, skipping the shared-lock probe on each.
    ///
    /// Returns one [`StringTokens`] per input, in order. The result for each
    /// string is byte-for-byte identical to calling [`intern`](Self::intern) on
    /// it individually (same shard, same slot order, same lower-companion
    /// interning) — this just drops the double-checked-locking probe, which is
    /// pure overhead on cache load where every string is a fresh miss.
    pub fn intern_batch<'a, I>(&self, it: I) -> Vec<StringTokens>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let it = it.into_iter();
        let mut out = Vec::with_capacity(it.size_hint().0);
        for s in it {
            out.push(self.intern_cold(s));
        }
        out
    }

    /// [`intern`](Self::intern) without the read-lock probe: goes straight to
    /// the owning shard's write lock.
    fn intern_cold(&self, s: &str) -> StringTokens {
        if s.is_empty() {
            return EMPTY_TOKENS;
        }
        let idx = shard_of(s);
        let mut guard = self.shards[idx].write();
        intern_locked(&mut guard, idx, s)
    }

    /// Run `f` while holding every shard's read lock, giving it a
    /// [`StringResolver`] that resolves `StringId`s to `&str` without per-call
    /// locking or cloning.
    ///
    /// Prefer this over many [`get_string`](Self::get_string) calls on paths
    /// (e.g. cache serialization) that resolve a large batch of ids. It blocks
    /// interning for its duration, so keep the closure short.
    pub fn with_read<R>(&self, f: impl FnOnce(StringResolver<'_>) -> R) -> R {
        f(StringResolver {
            guards: std::array::from_fn(|i| self.shards[i].read()),
        })
    }

    /// Retrieve the original (case‑preserving) text for a `StringId`.
    pub fn get_string(&self, id: StringId) -> Option<String> {
        self.with_string(id, str::to_string)
    }

    /// Borrow the original (case-preserving) text for a `StringId` without
    /// cloning it. Takes the owning shard's read lock once and calls `f` on the
    /// borrowed `&str`, returning `f`'s result (or `None` if the id is out of
    /// range).
    ///
    /// Prefer this over [`get_string`](Self::get_string) on hot paths that only
    /// need to compare or inspect the text (e.g. `== "NOT"`,
    /// `eq_ignore_ascii_case`): it avoids a per-call `String` allocation.
    pub fn with_string<R>(&self, id: StringId, f: impl FnOnce(&str) -> R) -> Option<R> {
        let (idx, slot) = split_id(id);
        let shard = self.shards[idx].read();
        shard.id_to_string.get(slot).map(|s| f(s.as_ref()))
    }

    /// Number of unique lower‑cased strings (not counting normal variants).
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.read().lower_map.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Approximate heap footprint of the interner, for profiling. Counts the
    /// `id_to_string` byte payload, the metadata array, and the two key maps'
    /// payloads. Pointer/control overhead is ignored, so this is a lower bound.
    pub fn stats(&self) -> StringTableStats {
        let mut out = StringTableStats::default();
        for shard in self.shards.iter() {
            let shard = shard.read();
            out.entries += shard.id_to_string.len();
            out.id_to_string_bytes += shard.id_to_string.iter().map(|s| s.len()).sum::<usize>();
            out.map_key_bytes += shard
                .lower_map
                .keys()
                .chain(shard.exact_map.keys())
                .map(|s| s.len())
                .sum::<usize>();
        }
        out
    }
}

/// Approximate per-component heap footprint of a [`StringTable`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StringTableStats {
    /// Number of slots across all shards (≈ interned strings, normal + lower).
    pub entries: usize,
    /// Total bytes of the interned string payloads.
    pub id_to_string_bytes: usize,
    /// Total bytes of the lower_map + exact_map key payloads.
    pub map_key_bytes: usize,
}

impl StringTableStats {
    /// Sum of all counted byte fields (a lower bound on heap use).
    pub fn total_bytes(&self) -> usize {
        self.id_to_string_bytes + self.map_key_bytes
    }
}

/// Borrowed resolver handed to [`StringTable::with_read`]. Holds every shard's
/// read lock for its lifetime so a batch of id lookups pays the locking cost
/// once.
pub struct StringResolver<'a> {
    guards: [RwLockReadGuard<'a, Shard>; SHARD_COUNT],
}

impl StringResolver<'_> {
    /// Resolve a `StringId` to its borrowed text, or `None` if out of range.
    pub fn get(&self, id: StringId) -> Option<&str> {
        let (idx, slot) = split_id(id);
        self.guards[idx].id_to_string.get(slot).map(|s| s.as_ref())
    }
}

/// Core interning logic, run with the owning shard's write lock already held.
/// Assumes `s` is non-empty (the empty-string slot-0 case is handled before
/// locking), that `idx` is `shard_of(s)`, and that the exact-string fast path
/// may or may not have been checked under a read lock — it re-checks
/// `exact_map` here so it is also correct when called directly under the write
/// lock (double-checked locking / batch interning).
fn intern_locked(shard: &mut Shard, idx: usize, s: &str) -> StringTokens {
    // Re-check after acquiring the write lock: another thread may have interned
    // this exact string in the gap (double-checked locking).
    if let Some(&existing) = shard.exact_map.get(s) {
        return existing;
    }

    let lower_key = s.to_lowercase();
    // Allocate each string once; share the same Arc between id_to_string and
    // the corresponding map key so there is only one heap allocation per string.
    let normal_arc: Arc<str> = Arc::from(s);
    let normal_id = shard.push(idx, &normal_arc);

    // Fast path 2: lower key exists → this is just a new casing of it.
    if let Some(&existing_lower) = shard.lower_map.get(lower_key.as_str()) {
        let token = StringTokens {
            lower: existing_lower.lower,
            normal: normal_id,
        };
        shard.exact_map.insert(normal_arc, token);
        return token;
    }

    // Slow path: brand‑new lower key, so it gets a slot of its own. Most keys
    // are already lower-case, and those two slots can share one allocation.
    let lower_arc: Arc<str> = if lower_key == s {
        Arc::clone(&normal_arc)
    } else {
        Arc::from(lower_key.as_str())
    };
    let lower_id = shard.push(idx, &lower_arc);

    let lower_token = StringTokens {
        lower: lower_id,
        normal: lower_id,
    };
    let normal_token = StringTokens {
        lower: lower_id,
        normal: normal_id,
    };

    shard.lower_map.insert(lower_arc, lower_token);
    shard.exact_map.insert(normal_arc, normal_token);
    normal_token
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn basic_interning() {
        let table = StringTable::new();
        let a = table.intern("hello");
        let b = table.intern("HELLO");
        let c = table.intern("hello");

        assert_eq!(a, c); // same exact string → same token
        assert_eq!(a.lower, b.lower); // same lower key → same lower ID
        assert_ne!(a.normal, b.normal); // different exact strings → different normal IDs

        assert_eq!(table.get_string(a.normal), Some("hello".to_string()));
        assert_eq!(table.get_string(b.normal), Some("HELLO".to_string()));
        assert_eq!(table.get_string(a.lower), Some("hello".to_string()));
    }

    /// Invariant: `lower` is the canonical case-folded id. `structural.rs`
    /// compares block keys against pre-interned keyword `lower` ids, so every
    /// spelling of a keyword has to collapse onto one id.
    #[test]
    fn lower_id_is_canonical_for_every_casing() {
        let table = StringTable::new();
        let a = table.intern("NOT");
        let b = table.intern("not");
        let c = table.intern("Not");
        let d = table.intern("nOt");

        assert_eq!(a.lower, b.lower);
        assert_eq!(a.lower, c.lower);
        assert_eq!(a.lower, d.lower);
        assert_eq!(table.get_string(a.lower).as_deref(), Some("not"));

        // Distinct spellings still keep distinct normal ids.
        let normals: HashSet<_> = [a.normal, b.normal, c.normal, d.normal].into();
        assert_eq!(normals.len(), 4);

        // Interning the lower form first must give the same answer.
        let t2 = StringTable::new();
        let e = t2.intern("else_if");
        let f = t2.intern("ELSE_IF");
        let g = t2.intern("Else_If");
        assert_eq!(e.lower, f.lower);
        assert_eq!(e.lower, g.lower);
        assert_eq!(t2.get_string(e.lower).as_deref(), Some("else_if"));
    }

    /// The same invariant across enough keys that they land in many different
    /// shards — a shard chosen from the exact bytes rather than the folded ones
    /// would split casings apart and this would fail.
    #[test]
    fn lower_id_is_canonical_across_shards() {
        let table = StringTable::new();
        let mut shards = HashSet::new();
        for i in 0..2000 {
            let lower = format!("some_key_{i}_suffix");
            let upper = lower.to_uppercase();
            let mixed = format!("Some_Key_{i}_Suffix");

            let a = table.intern(&lower);
            let b = table.intern(&upper);
            let c = table.intern(&mixed);
            assert_eq!(a.lower, b.lower, "{lower}");
            assert_eq!(a.lower, c.lower, "{lower}");
            assert_eq!(table.get_string(a.lower).as_deref(), Some(lower.as_str()));

            shards.insert(split_id(a.lower).0);
            assert_eq!(split_id(b.normal).0, split_id(a.lower).0);
            assert_eq!(split_id(c.normal).0, split_id(a.lower).0);
        }
        // Sharding actually spreads; a degenerate hash would defeat the point.
        assert_eq!(shards.len(), SHARD_COUNT, "ids clustered into few shards");
    }

    /// Non-ASCII folding takes the allocating arm of `shard_of`; it still has to
    /// agree with the ASCII arm wherever the two can produce the same bytes.
    #[test]
    fn non_ascii_case_folding_shares_one_lower_id() {
        let table = StringTable::new();

        // Kelvin sign lower-cases to plain ASCII "k", crossing the two arms.
        assert_eq!(table.intern("\u{212A}").lower, table.intern("k").lower);
        assert_eq!(
            table.get_string(table.intern("\u{212A}").lower).as_deref(),
            Some("k")
        );

        // Final sigma: str::to_lowercase("ΑΣ") is "ας", not "ασ".
        assert_eq!(table.intern("ΑΣ").lower, table.intern("ας").lower);

        assert_eq!(table.intern("ÉCOLE").lower, table.intern("école").lower);
        assert_eq!(table.intern("Straße").lower, table.intern("STRAßE").lower);
    }

    /// Invariant: interning the same string from many threads yields one id.
    #[test]
    fn concurrent_intern_is_idempotent() {
        const THREADS: usize = 16;
        const WORDS: usize = 400;

        let words: Vec<String> = (0..WORDS).map(|i| format!("shared_key_{i}")).collect();
        // Three spellings each, so the lower-companion path races too.
        let inputs: Vec<String> = words
            .iter()
            .flat_map(|w| {
                [
                    w.clone(),
                    w.to_uppercase(),
                    format!("{}{}", w[..1].to_uppercase(), &w[1..]),
                ]
            })
            .collect();
        let n = inputs.len();

        let table = StringTable::new();
        // Each thread starts at a different offset so they collide on the same
        // strings instead of partitioning them.
        let stride = 37;
        let results: Vec<Vec<StringTokens>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|t| {
                    let table = table.clone();
                    let inputs = &inputs;
                    scope.spawn(move || {
                        (0..n)
                            .map(|i| table.intern(&inputs[(i + t * stride) % n]))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let mut normal_of: HashMap<&str, StringId> = HashMap::new();
        let mut lower_of: HashMap<String, StringId> = HashMap::new();
        for (t, tokens) in results.iter().enumerate() {
            for (i, tok) in tokens.iter().enumerate() {
                let s = &inputs[(i + t * stride) % n];
                assert_eq!(
                    *normal_of.entry(s.as_str()).or_insert(tok.normal),
                    tok.normal,
                    "two normal ids for {s}"
                );
                assert_eq!(
                    *lower_of.entry(s.to_lowercase()).or_insert(tok.lower),
                    tok.lower,
                    "two lower ids for {s}"
                );
                assert_eq!(table.get_string(tok.normal).as_deref(), Some(s.as_str()));
                assert_eq!(
                    table.get_string(tok.lower),
                    Some(s.to_lowercase()),
                    "lower text for {s}"
                );
            }
        }
        assert_eq!(normal_of.len(), n);
        assert_eq!(lower_of.len(), WORDS);
        assert_eq!(table.len(), WORDS);
    }

    #[test]
    fn empty_string_owns_id_zero() {
        let table = StringTable::new();
        assert_eq!(table.intern(""), EMPTY_TOKENS);
        assert_eq!(table.get_string(StringId(0)).as_deref(), Some(""));
        for i in 0..500 {
            let t = table.intern(&format!("key_{i}"));
            assert_ne!(t.normal, StringId(0));
            assert_ne!(t.lower, StringId(0));
        }
    }

    #[test]
    fn out_of_range_ids_resolve_to_none() {
        let table = StringTable::new();
        for i in 0..500 {
            table.intern(&format!("key_{i}"));
        }
        // A slot past the end of shard 0, of the last shard, and the reserved
        // u32::MAX must all miss rather than hit a neighbouring shard's text.
        for id in [
            StringId(9_999),
            StringId(SLOT_MASK),
            StringId(u32::MAX),
            StringId(u32::MAX - 1),
        ] {
            assert_eq!(table.with_string(id, |_| true), None, "{id:?}");
            assert_eq!(table.get_string(id), None, "{id:?}");
            table.with_read(|r| assert_eq!(r.get(id), None, "{id:?}"));
        }
    }

    #[test]
    fn with_string_borrows_without_clone() {
        let table = StringTable::new();
        let a = table.intern("NOT");
        // Borrow + compare without allocating an owned String.
        assert_eq!(table.with_string(a.normal, |s| s == "NOT"), Some(true));
        assert_eq!(
            table.with_string(a.lower, |s| s.eq_ignore_ascii_case("not")),
            Some(true)
        );
        // Out-of-range id yields None and never calls the closure.
        assert_eq!(table.with_string(StringId(9_999), |_| true), None);
        // Same text as get_string.
        assert_eq!(
            table.with_string(a.normal, |s| s.to_string()),
            table.get_string(a.normal)
        );
    }

    #[test]
    fn intern_batch_matches_per_string() {
        // A fresh table built via intern_batch must hand out byte-identical
        // tokens (same ids, same order) to one built with per-string intern.
        let inputs = [
            "foo", "FOO", "foo", "bar", "Bar", "", "\"q\"", "baz", "FOO", "bar",
        ];

        let single = StringTable::new();
        let want: Vec<_> = inputs.iter().map(|s| single.intern(s)).collect();

        let batch = StringTable::new();
        let got = batch.intern_batch(inputs.iter().copied());

        assert_eq!(want, got);
        // And the resolved text agrees for every id.
        for (a, b) in want.iter().zip(got.iter()) {
            assert_eq!(single.get_string(a.normal), batch.get_string(b.normal));
            assert_eq!(single.get_string(a.lower), batch.get_string(b.lower));
        }
    }

    /// Two independently built tables must agree on ids for the same input
    /// sequence: `crates/cache`'s roundtrip test compares tokens across tables.
    #[test]
    fn id_assignment_is_reproducible_across_tables() {
        let inputs: Vec<String> = (0..1000)
            .flat_map(|i| [format!("word_{i}"), format!("WORD_{i}")])
            .collect();

        let a = StringTable::new();
        let b = StringTable::new();
        let want: Vec<_> = inputs.iter().map(|s| a.intern(s)).collect();
        let got: Vec<_> = inputs.iter().map(|s| b.intern(s)).collect();
        assert_eq!(want, got);
    }

    #[test]
    fn with_read_resolves_without_per_call_lock() {
        let table = StringTable::new();
        let a = table.intern("hello");
        let b = table.intern("WORLD");
        table.with_read(|r| {
            assert_eq!(r.get(a.normal), Some("hello"));
            assert_eq!(r.get(b.normal), Some("WORLD"));
            assert_eq!(r.get(StringId(9_999)), None);
        });
    }

    #[test]
    fn shared_table() {
        let table = StringTable::new();
        let a = table.intern("hello");

        let table2 = table.clone();
        let b = table2.intern("hello");

        assert_eq!(a, b); // shared table → same token
    }

    #[test]
    fn independent_tables_do_not_share_entries() {
        let a = StringTable::new();
        let token = a.intern("only_in_a");
        assert_eq!(a.len(), 1);

        let b = StringTable::new();
        assert!(b.is_empty());
        assert_eq!(b.get_string(token.normal), None);

        b.intern("only_in_b");
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a.get_string(token.normal).as_deref(), Some("only_in_a"));
    }

    #[test]
    fn cloned_handle_does_not_leak_into_a_fresh_table() {
        let a = StringTable::new();
        let shared = a.clone();
        shared.intern("via_clone");
        assert_eq!(a.len(), 1);

        let fresh = StringTable::new();
        assert!(fresh.is_empty());
    }

    #[test]
    fn stats_cover_every_shard() {
        let table = StringTable::new();
        for i in 0..2000 {
            table.intern(&format!("stats_key_{i}"));
        }
        let stats = table.stats();
        assert_eq!(table.len(), 2000);
        // 2000 lower slots + 2000 normal slots + one empty slot per shard.
        assert_eq!(stats.entries, 4000 + SHARD_COUNT);
        assert!(stats.total_bytes() > 0);
    }
}
