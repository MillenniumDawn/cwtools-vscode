// stripped to WHY-only — see git history for full docs (shard/cache-line notes kept in code structure)
use parking_lot::{RwLock, RwLockReadGuard};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::Hasher;
use std::sync::Arc;

const SHARD_BITS: u32 = 6;
const SHARD_COUNT: usize = 1 << SHARD_BITS;
const SLOT_BITS: u32 = u32::BITS - SHARD_BITS;
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct StringId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringTokens {
    pub lower: StringId,
    pub normal: StringId,
}

const EMPTY_TOKENS: StringTokens = StringTokens {
    lower: StringId(0),
    normal: StringId(0),
};

#[inline]
fn split_id(id: StringId) -> (usize, usize) {
    ((id.0 >> SLOT_BITS) as usize, (id.0 & SLOT_MASK) as usize)
}

#[inline]
fn shard_of(s: &str) -> usize {
    let mut h = FxHasher::default();
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

#[repr(align(64))]
struct Shard {
    lower_map: FxHashMap<Arc<str>, StringTokens>,
    exact_map: FxHashMap<Arc<str>, StringTokens>,
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

pub struct StringTable {
    shards: Arc<[RwLock<Shard>; SHARD_COUNT]>,
}

impl Clone for StringTable {
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

    pub fn intern(&self, s: &str) -> StringTokens {
        if s.is_empty() {
            return EMPTY_TOKENS;
        }

        let idx = shard_of(s);
        let shard = &self.shards[idx];

        {
            let guard = shard.read();
            if let Some(&existing) = guard.exact_map.get(s) {
                return existing;
            }
        }

        let mut guard = shard.write();
        intern_locked(&mut guard, idx, s)
    }

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

    fn intern_cold(&self, s: &str) -> StringTokens {
        if s.is_empty() {
            return EMPTY_TOKENS;
        }
        let idx = shard_of(s);
        let mut guard = self.shards[idx].write();
        intern_locked(&mut guard, idx, s)
    }

    pub fn with_read<R>(&self, f: impl FnOnce(StringResolver<'_>) -> R) -> R {
        f(StringResolver {
            guards: std::array::from_fn(|i| self.shards[i].read()),
        })
    }

    pub fn get_string(&self, id: StringId) -> Option<String> {
        self.with_string(id, str::to_string)
    }

    pub fn with_string<R>(&self, id: StringId, f: impl FnOnce(&str) -> R) -> Option<R> {
        let (idx, slot) = split_id(id);
        let shard = self.shards[idx].read();
        shard.id_to_string.get(slot).map(|s| f(s.as_ref()))
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.read().lower_map.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

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

#[derive(Debug, Clone, Copy, Default)]
pub struct StringTableStats {
    pub entries: usize,
    pub id_to_string_bytes: usize,
    pub map_key_bytes: usize,
}

impl StringTableStats {
    pub fn total_bytes(&self) -> usize {
        self.id_to_string_bytes + self.map_key_bytes
    }
}

pub struct StringResolver<'a> {
    guards: [RwLockReadGuard<'a, Shard>; SHARD_COUNT],
}

impl StringResolver<'_> {
    pub fn get(&self, id: StringId) -> Option<&str> {
        let (idx, slot) = split_id(id);
        self.guards[idx].id_to_string.get(slot).map(|s| s.as_ref())
    }
}

fn intern_locked(shard: &mut Shard, idx: usize, s: &str) -> StringTokens {
    if let Some(&existing) = shard.exact_map.get(s) {
        return existing;
    }

    let lower_key = s.to_lowercase();
    let normal_arc: Arc<str> = Arc::from(s);
    let normal_id = shard.push(idx, &normal_arc);

    if let Some(&existing_lower) = shard.lower_map.get(lower_key.as_str()) {
        let token = StringTokens {
            lower: existing_lower.lower,
            normal: normal_id,
        };
        shard.exact_map.insert(normal_arc, token);
        return token;
    }

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

        assert_eq!(a, c);
        assert_eq!(a.lower, b.lower);
        assert_ne!(a.normal, b.normal);

        assert_eq!(table.get_string(a.normal), Some("hello".to_string()));
        assert_eq!(table.get_string(b.normal), Some("HELLO".to_string()));
        assert_eq!(table.get_string(a.lower), Some("hello".to_string()));
    }

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

        let normals: HashSet<_> = [a.normal, b.normal, c.normal, d.normal].into();
        assert_eq!(normals.len(), 4);

        let t2 = StringTable::new();
        let e = t2.intern("else_if");
        let f = t2.intern("ELSE_IF");
        let g = t2.intern("Else_If");
        assert_eq!(e.lower, f.lower);
        assert_eq!(e.lower, g.lower);
        assert_eq!(t2.get_string(e.lower).as_deref(), Some("else_if"));
    }

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
        assert_eq!(shards.len(), SHARD_COUNT, "ids clustered into few shards");
    }

    #[test]
    fn non_ascii_case_folding_shares_one_lower_id() {
        let table = StringTable::new();

        assert_eq!(table.intern("\u{212A}").lower, table.intern("k").lower);
        assert_eq!(
            table.get_string(table.intern("\u{212A}").lower).as_deref(),
            Some("k")
        );

        assert_eq!(table.intern("ΑΣ").lower, table.intern("ας").lower);

        assert_eq!(table.intern("ÉCOLE").lower, table.intern("école").lower);
        assert_eq!(table.intern("Straße").lower, table.intern("STRAßE").lower);
    }

    #[test]
    fn concurrent_intern_is_idempotent() {
        const THREADS: usize = 16;
        const WORDS: usize = 400;

        let words: Vec<String> = (0..WORDS).map(|i| format!("shared_key_{i}")).collect();
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
        assert_eq!(table.with_string(a.normal, |s| s == "NOT"), Some(true));
        assert_eq!(
            table.with_string(a.lower, |s| s.eq_ignore_ascii_case("not")),
            Some(true)
        );
        assert_eq!(table.with_string(StringId(9_999), |_| true), None);
        assert_eq!(
            table.with_string(a.normal, |s| s.to_string()),
            table.get_string(a.normal)
        );
    }

    #[test]
    fn intern_batch_matches_per_string() {
        let inputs = [
            "foo", "FOO", "foo", "bar", "Bar", "", "\"q\"", "baz", "FOO", "bar",
        ];

        let single = StringTable::new();
        let want: Vec<_> = inputs.iter().map(|s| single.intern(s)).collect();

        let batch = StringTable::new();
        let got = batch.intern_batch(inputs.iter().copied());

        assert_eq!(want, got);
        for (a, b) in want.iter().zip(got.iter()) {
            assert_eq!(single.get_string(a.normal), batch.get_string(b.normal));
            assert_eq!(single.get_string(a.lower), batch.get_string(b.lower));
        }
    }

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

        assert_eq!(a, b);
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
        assert_eq!(stats.entries, 4000 + SHARD_COUNT);
        assert!(stats.total_bytes() > 0);
    }
}
