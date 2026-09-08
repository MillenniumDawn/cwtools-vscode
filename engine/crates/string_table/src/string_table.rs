// stripped to WHY-only — see git history for full docs (shard/cache-line notes kept in code structure)
use parking_lot::{RwLock, RwLockReadGuard};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::Hasher;
use std::sync::{Arc, Weak};

const SHARD_BITS: u32 = 6;
const SHARD_COUNT: usize = 1 << SHARD_BITS;
const SLOT_BITS: u32 = u32::BITS - SHARD_BITS;
const SLOT_MASK: u32 = (1 << SLOT_BITS) - 1;

// Overlay ids are tagged with the top bit of the slot field. Base `push` keeps
// slots below it, so the tag is unambiguous whatever a base id's shard bits are.
const OVERLAY_BIT: u32 = 1 << (SLOT_BITS - 1);
// An overlay id carries no shard, so those 6 bits become the generation. The
// remaining 25 slot bits split into region index and entry index. 256 regions
// covers the LSP's 128 open documents plus its speculative-parse slot, and 128K
// entries is far above the few hundred strings a document has that the base
// table has never seen. Both limits fall back to base interning on overflow.
const OVERLAY_GEN_SHIFT: u32 = SLOT_BITS;
const OVERLAY_GEN_MASK: u32 = (1 << (u32::BITS - SLOT_BITS)) - 1;
const OVERLAY_REGION_BITS: u32 = 8;
const OVERLAY_INDEX_BITS: u32 = SLOT_BITS - 1 - OVERLAY_REGION_BITS;
const OVERLAY_INDEX_MASK: u32 = (1 << OVERLAY_INDEX_BITS) - 1;
const OVERLAY_REGION_MASK: u32 = (1 << OVERLAY_REGION_BITS) - 1;
const MAX_OVERLAY_REGIONS: usize = 1 << OVERLAY_REGION_BITS;
const MAX_OVERLAY_ENTRIES: usize = 1 << OVERLAY_INDEX_BITS;

#[inline]
fn is_overlay(id: StringId) -> bool {
    id.0 & OVERLAY_BIT != 0
}

#[inline]
fn split_overlay_id(id: StringId) -> (u32, u32, usize) {
    (
        (id.0 >> OVERLAY_GEN_SHIFT) & OVERLAY_GEN_MASK,
        (id.0 >> OVERLAY_INDEX_BITS) & OVERLAY_REGION_MASK,
        (id.0 & OVERLAY_INDEX_MASK) as usize,
    )
}

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
        // Hard assert, not debug: overflowing now sets the overlay tag, so a
        // release build would alias a live region instead of merely corrupting
        // the shard bits. One compare on the cold intern path.
        assert!(
            slot < OVERLAY_BIT,
            "StringTable shard id space exhausted (the top slot bit tags overlay ids)"
        );
        self.id_to_string.push(Arc::clone(text));
        StringId(((shard as u32) << SLOT_BITS) | slot)
    }
}

/// A per-parse interner for strings the base table has never seen. Dropping the
/// last handle frees every entry, which is what keeps half-typed identifiers from
/// accumulating for the process lifetime (#475).
struct OverlayRegion {
    region: u32,
    generation: u32,
    inner: RwLock<OverlayInner>,
}

#[derive(Default)]
struct OverlayInner {
    lower_map: FxHashMap<Arc<str>, StringTokens>,
    exact_map: FxHashMap<Arc<str>, StringTokens>,
    id_to_string: Vec<Arc<str>>,
}

impl OverlayRegion {
    fn push(&self, inner: &mut OverlayInner, text: &Arc<str>) -> StringId {
        let index = inner.id_to_string.len() as u32;
        inner.id_to_string.push(Arc::clone(text));
        StringId(
            OVERLAY_BIT
                | (self.generation << OVERLAY_GEN_SHIFT)
                | (self.region << OVERLAY_INDEX_BITS)
                | index,
        )
    }

    /// Mirrors `intern_locked`, except the canonical lower id comes from the base
    /// table when it already knows this spelling case-folded — otherwise every
    /// casing of a word would stop sharing one `lower` id. `None` means the region
    /// is full and the caller should fall back to the base table.
    fn intern(
        &self,
        s: &str,
        lower_key: &str,
        base_lower: Option<StringId>,
    ) -> Option<StringTokens> {
        let mut inner = self.inner.write();
        if let Some(&existing) = inner.exact_map.get(s) {
            return Some(existing);
        }

        let needs_lower = base_lower.is_none() && !inner.lower_map.contains_key(lower_key);
        let needed = if needs_lower { 2 } else { 1 };
        if inner.id_to_string.len() + needed > MAX_OVERLAY_ENTRIES {
            return None;
        }

        let normal_arc: Arc<str> = Arc::from(s);
        let normal_id = self.push(&mut inner, &normal_arc);

        let lower_id = match base_lower {
            Some(id) => id,
            None => match inner.lower_map.get(lower_key) {
                Some(&existing) => existing.lower,
                None => {
                    let lower_arc: Arc<str> = if lower_key == s {
                        Arc::clone(&normal_arc)
                    } else {
                        Arc::from(lower_key)
                    };
                    let id = self.push(&mut inner, &lower_arc);
                    inner.lower_map.insert(
                        lower_arc,
                        StringTokens {
                            lower: id,
                            normal: id,
                        },
                    );
                    id
                }
            },
        };

        let token = StringTokens {
            lower: lower_id,
            normal: normal_id,
        };
        inner.exact_map.insert(normal_arc, token);
        Some(token)
    }
}

struct OverlaySlot {
    generation: u32,
    region: Weak<OverlayRegion>,
}

/// Keeps an overlay region alive. A parsed AST holds one so its ids stay
/// resolvable for exactly as long as the AST itself.
#[derive(Clone)]
pub struct OverlayGuard(Arc<OverlayRegion>);

impl OverlayGuard {
    /// How many strings this region holds that the base table did not already have.
    pub fn entries(&self) -> usize {
        self.0.inner.read().id_to_string.len()
    }
}

/// LOCK ORDER: shard, then the overlay slab, then a region. `with_overlay` takes
/// only the slab, so it cannot invert. Interning inside a `with_read` closure
/// still self-deadlocks on the shard guards.
pub struct StringTable {
    shards: Arc<[RwLock<Shard>; SHARD_COUNT]>,
    overlays: Arc<RwLock<Vec<OverlaySlot>>>,
    overlay: Option<Arc<OverlayRegion>>,
}

impl Clone for StringTable {
    fn clone(&self) -> Self {
        Self {
            shards: Arc::clone(&self.shards),
            overlays: Arc::clone(&self.overlays),
            overlay: self.overlay.clone(),
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
            overlays: Arc::new(RwLock::new(Vec::new())),
            overlay: None,
        }
    }

    /// A handle over the same base table whose *new* strings land in a private
    /// region instead of growing the base table. Ids from either side resolve
    /// through any handle, so only the parse site needs this one; hold the
    /// [`OverlayGuard`] for as long as the ids are in use. Falls back to a plain
    /// handle when every region slot is taken.
    pub fn with_overlay(&self) -> Self {
        let mut slab = self.overlays.write();
        let free = slab.iter().position(|s| s.region.strong_count() == 0);
        let index = match free {
            Some(i) => {
                slab[i].generation = (slab[i].generation + 1) & OVERLAY_GEN_MASK;
                i
            }
            None => {
                if slab.len() >= MAX_OVERLAY_REGIONS {
                    drop(slab);
                    return Self {
                        shards: Arc::clone(&self.shards),
                        overlays: Arc::clone(&self.overlays),
                        overlay: None,
                    };
                }
                slab.push(OverlaySlot {
                    generation: 0,
                    region: Weak::new(),
                });
                slab.len() - 1
            }
        };
        let region = Arc::new(OverlayRegion {
            region: index as u32,
            generation: slab[index].generation,
            inner: RwLock::new(OverlayInner::default()),
        });
        slab[index].region = Arc::downgrade(&region);
        drop(slab);
        Self {
            shards: Arc::clone(&self.shards),
            overlays: Arc::clone(&self.overlays),
            overlay: Some(region),
        }
    }

    /// The guard for this handle's region, to be stored alongside anything holding
    /// ids it interned. `None` for a plain handle.
    pub fn overlay_guard(&self) -> Option<OverlayGuard> {
        self.overlay.as_ref().map(|r| OverlayGuard(Arc::clone(r)))
    }

    /// This table's base storage bound to `guard`'s region, so strings interned
    /// while working on an AST land in the same space as that AST's own ids
    /// instead of growing the base table.
    pub fn rebound(&self, guard: Option<&OverlayGuard>) -> Self {
        Self {
            shards: Arc::clone(&self.shards),
            overlays: Arc::clone(&self.overlays),
            overlay: guard.map(|g| Arc::clone(&g.0)),
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

        if let Some(region) = &self.overlay {
            let lower_key = s.to_lowercase();
            let base_lower = {
                let guard = shard.read();
                guard.lower_map.get(lower_key.as_str()).map(|t| t.lower)
            };
            if let Some(tokens) = region.intern(s, &lower_key, base_lower) {
                return tokens;
            }
        }

        let mut guard = shard.write();
        intern_locked(&mut guard, idx, s)
    }

    fn overlay_region(&self, id: StringId) -> Option<Arc<OverlayRegion>> {
        let (generation, region, _) = split_overlay_id(id);
        if let Some(own) = &self.overlay
            && own.region == region
            && own.generation == generation
        {
            return Some(Arc::clone(own));
        }
        let slab = self.overlays.read();
        let slot = slab.get(region as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.region.upgrade()
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
        if self.overlay.is_some() {
            return self.intern(s);
        }
        let idx = shard_of(s);
        let mut guard = self.shards[idx].write();
        intern_locked(&mut guard, idx, s)
    }

    pub fn with_read<R>(&self, f: impl FnOnce(StringResolver<'_>) -> R) -> R {
        // Snapshotting the live regions' `Arc<str>` handles keeps the resolver's
        // borrows tied to itself rather than to a region lock. It costs nothing
        // when no overlay exists, which is every batch (CLI/scan) caller.
        let overlays = {
            let slab = self.overlays.read();
            slab.iter()
                .map(|slot| {
                    slot.region
                        .upgrade()
                        .map(|r| (slot.generation, r.inner.read().id_to_string.clone()))
                })
                .collect()
        };
        f(StringResolver {
            guards: std::array::from_fn(|i| self.shards[i].read()),
            overlays,
        })
    }

    pub fn get_string(&self, id: StringId) -> Option<String> {
        self.with_string(id, str::to_string)
    }

    pub fn with_string<R>(&self, id: StringId, f: impl FnOnce(&str) -> R) -> Option<R> {
        if is_overlay(id) {
            let region = self.overlay_region(id)?;
            let (_, _, index) = split_overlay_id(id);
            let inner = region.inner.read();
            return inner.id_to_string.get(index).map(|s| f(s.as_ref()));
        }
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

    /// Base-table figures only; `overlay_*` covers the live regions separately so
    /// a growing entry count still means the permanent table is growing.
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
        for slot in self.overlays.read().iter() {
            if let Some(region) = slot.region.upgrade() {
                out.overlay_regions += 1;
                out.overlay_entries += region.inner.read().id_to_string.len();
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StringTableStats {
    pub entries: usize,
    pub id_to_string_bytes: usize,
    pub map_key_bytes: usize,
    pub overlay_regions: usize,
    pub overlay_entries: usize,
}

impl StringTableStats {
    pub fn total_bytes(&self) -> usize {
        self.id_to_string_bytes + self.map_key_bytes
    }
}

pub struct StringResolver<'a> {
    guards: [RwLockReadGuard<'a, Shard>; SHARD_COUNT],
    overlays: Vec<Option<(u32, Vec<Arc<str>>)>>,
}

impl StringResolver<'_> {
    pub fn get(&self, id: StringId) -> Option<&str> {
        if is_overlay(id) {
            let (generation, region, index) = split_overlay_id(id);
            let (live_generation, entries) = self.overlays.get(region as usize)?.as_ref()?;
            if *live_generation != generation {
                return None;
            }
            return entries.get(index).map(|s| s.as_ref());
        }
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
    fn overlay_reuses_base_ids_and_keeps_new_strings_out_of_base() {
        let table = StringTable::new();
        let known = table.intern("known_key");
        let base_entries = table.stats().entries;

        let scratch = table.with_overlay();
        assert_eq!(scratch.intern("known_key"), known, "base hit must win");

        let novel = scratch.intern("half_typed_ident");
        assert!(
            is_overlay(novel.normal),
            "novel string must land in the overlay"
        );
        assert_eq!(
            table.stats().entries,
            base_entries,
            "the overlay must not grow the base table"
        );
        assert_eq!(table.stats().overlay_regions, 1);

        // Resolvable through the plain handle, not just the overlay handle.
        assert_eq!(
            table.get_string(novel.normal).as_deref(),
            Some("half_typed_ident")
        );
        table.with_read(|r| assert_eq!(r.get(novel.normal), Some("half_typed_ident")));
    }

    #[test]
    fn overlay_shares_one_lower_id_with_the_base_table() {
        let table = StringTable::new();
        let base = table.intern("SomeKey");
        let scratch = table.with_overlay();

        // A casing the base table has never seen still has to fold to the base
        // lower id, or `.lower` stops being canonical identity.
        let other = scratch.intern("SOMEKEY");
        assert_eq!(other.lower, base.lower);
        assert_ne!(other.normal, base.normal);
        assert!(is_overlay(other.normal));
        assert!(!is_overlay(other.lower));

        // Two casings that are both novel share one overlay lower id.
        let a = scratch.intern("BrandNew");
        let b = scratch.intern("brandnew");
        assert_eq!(a.lower, b.lower);
        assert_eq!(scratch.get_string(a.lower).as_deref(), Some("brandnew"));
    }

    #[test]
    fn dropping_the_last_handle_reclaims_the_region() {
        let table = StringTable::new();
        let base_entries = table.stats().entries;

        let novel = {
            let scratch = table.with_overlay();
            let novel = scratch.intern("only_while_open");
            let guard = scratch.overlay_guard().expect("overlay handle has a guard");
            assert_eq!(guard.entries(), 2, "one normal id and one lower id");
            assert_eq!(
                table.get_string(novel.normal).as_deref(),
                Some("only_while_open")
            );
            novel
        };

        assert_eq!(
            table.stats().overlay_regions,
            0,
            "region freed with its handle"
        );
        assert_eq!(
            table.stats().entries,
            base_entries,
            "and nothing leaked to base"
        );
        assert_eq!(table.get_string(novel.normal), None);
        table.with_read(|r| assert_eq!(r.get(novel.normal), None));
    }

    #[test]
    fn a_guard_keeps_the_region_alive_after_its_handle_drops() {
        let table = StringTable::new();
        let (novel, guard) = {
            let scratch = table.with_overlay();
            (
                scratch.intern("held_by_guard"),
                scratch.overlay_guard().unwrap(),
            )
        };
        assert_eq!(
            table.get_string(novel.normal).as_deref(),
            Some("held_by_guard")
        );
        drop(guard);
        assert_eq!(table.get_string(novel.normal), None);
    }

    #[test]
    fn a_recycled_region_slot_does_not_alias_stale_ids() {
        let table = StringTable::new();
        let stale = {
            let scratch = table.with_overlay();
            scratch.intern("first_tenant")
        };
        assert_eq!(table.get_string(stale.normal), None);

        let scratch = table.with_overlay();
        let fresh = scratch.intern("second_tenant");
        assert_eq!(
            split_overlay_id(stale.normal).1,
            split_overlay_id(fresh.normal).1,
            "the slot should have been recycled"
        );
        assert_ne!(stale.normal, fresh.normal, "but the generation must differ");
        assert_eq!(table.get_string(stale.normal), None);
        assert_eq!(
            table.get_string(fresh.normal).as_deref(),
            Some("second_tenant")
        );
    }

    #[test]
    fn overlays_are_isolated_from_each_other() {
        let table = StringTable::new();
        let a = table.with_overlay();
        let b = table.with_overlay();

        let in_a = a.intern("only_in_a");
        let in_b = b.intern("only_in_b");
        assert_ne!(in_a.normal, in_b.normal);

        // Each handle resolves the other's ids through the shared slab.
        assert_eq!(a.get_string(in_b.normal).as_deref(), Some("only_in_b"));
        assert_eq!(b.get_string(in_a.normal).as_deref(), Some("only_in_a"));
        assert_eq!(table.stats().overlay_regions, 2);
    }

    #[test]
    fn slab_exhaustion_falls_back_to_the_base_table() {
        let table = StringTable::new();
        let held: Vec<StringTable> = (0..MAX_OVERLAY_REGIONS)
            .map(|_| table.with_overlay())
            .collect();
        assert!(held.iter().all(|t| t.overlay_guard().is_some()));

        let spilled = table.with_overlay();
        assert!(spilled.overlay_guard().is_none(), "no region left to claim");
        let tokens = spilled.intern("spills_into_base");
        assert!(!is_overlay(tokens.normal));
        assert_eq!(
            table.get_string(tokens.normal).as_deref(),
            Some("spills_into_base")
        );
    }

    #[test]
    fn a_full_region_falls_back_to_the_base_table() {
        let table = StringTable::new();
        let scratch = table.with_overlay();
        let region = scratch.overlay.as_ref().unwrap();

        // Fill the region to its last slot rather than interning 128K strings.
        {
            let mut inner = region.inner.write();
            let filler: Arc<str> = Arc::from("filler");
            inner
                .id_to_string
                .resize(MAX_OVERLAY_ENTRIES - 1, Arc::clone(&filler));
        }

        let tokens = scratch.intern("does_not_fit");
        assert!(
            !is_overlay(tokens.normal),
            "a novel string needing two slots must fall back to base"
        );
        assert_eq!(
            table.get_string(tokens.normal).as_deref(),
            Some("does_not_fit")
        );
    }

    #[test]
    fn overlay_interning_is_idempotent() {
        let table = StringTable::new();
        let scratch = table.with_overlay();
        let a = scratch.intern("repeated");
        let b = scratch.intern("repeated");
        assert_eq!(a, b);
        assert_eq!(scratch.overlay_guard().unwrap().entries(), 2);
        assert_eq!(scratch.intern_batch(["repeated"]), vec![a]);
        assert_eq!(scratch.overlay_guard().unwrap().entries(), 2);
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
