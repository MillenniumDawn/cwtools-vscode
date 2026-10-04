//! Where every loc key is defined: the goto target per key, and behind it
//! every other definition site the loc scan parsed, so find-references can
//! list the definition half without walking the localisation tree (#474).
//!
//! A site is `(file id, line0)`, eight bytes; keys are the `LocIndex`'s
//! interned `Arc<str>`s, so the map adds no string memory of its own. Sites
//! are kept per key with the goto target first: the last primary-language
//! file scanned wins that slot, everything else keeps scan order, and a
//! base-game site sits at the tail, so it is the goto target only while the
//! workspace has no definition of its own (the winners the one-per-key map
//! had before this) and takes over again when the last one is removed.
//!
//! Removing a file tombstones its id instead of walking the map: readers skip
//! sites of a dead file, and the sweep that actually drops them runs once the
//! dead sites are a noticeable share of the map, not once per event. Removed
//! URIs are released immediately; IDs are recycled only after their sites go.

use std::sync::Arc;

use rustc_hash::FxHashMap;

/// Sweep when the dead sites exceed this share of all recorded sites; until
/// then they cost eight bytes each and one branch per lookup.
const SWEEP_DEAD_SHARE: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Site {
    file: u32,
    line: u32,
}

#[derive(Debug)]
struct File {
    /// None once removed; its sites remain tombstoned until the next sweep.
    uri: Option<Arc<str>>,
    vanilla: bool,
    /// Sites recorded for this file, so a removal knows what it tombstoned.
    sites: u32,
}

#[derive(Default, Debug)]
pub(crate) struct LocLocations {
    files: Vec<File>,
    file_ids: FxHashMap<Arc<str>, u32>,
    /// Removed IDs that may still have sites. Never reuse before sweeping.
    retired: Vec<u32>,
    /// IDs with no remaining sites; safe to assign to any URI.
    free: Vec<u32>,
    by_key: FxHashMap<Arc<str>, Vec<Site>>,
    total_sites: u32,
    dead_sites: u32,
}

impl LocLocations {
    fn file_id(&mut self, uri: &Arc<str>, vanilla: bool) -> u32 {
        if let Some(&id) = self.file_ids.get(uri) {
            return id;
        }
        let file = File {
            uri: Some(Arc::clone(uri)),
            vanilla,
            sites: 0,
        };
        let id = if let Some(id) = self.free.pop() {
            self.files[id as usize] = file;
            id
        } else {
            let id = u32::try_from(self.files.len()).expect("too many localisation files");
            self.files.push(file);
            id
        };
        self.file_ids.insert(Arc::clone(uri), id);
        id
    }

    fn site(&mut self, uri: &Arc<str>, line0: u32, vanilla: bool) -> Site {
        let file = self.file_id(uri, vanilla);
        self.files[file as usize].sites += 1;
        self.total_sites += 1;
        Site { file, line: line0 }
    }

    fn live(&self, site: &Site) -> bool {
        self.files[site.file as usize].uri.is_some()
    }

    /// Records a workspace site. A primary-language site becomes the goto
    /// target (the last one recorded wins); any other language appends behind
    /// the sites already there, but ahead of a base-game fallback.
    pub(crate) fn insert(&mut self, key: Arc<str>, uri: &Arc<str>, line0: u32, primary_lang: bool) {
        let site = self.site(uri, line0, false);
        let files = &self.files;
        let sites = self.by_key.entry(key).or_default();
        if primary_lang {
            sites.insert(0, site);
        } else {
            let at = sites
                .iter()
                .position(|s| files[s.file as usize].vanilla)
                .unwrap_or(sites.len());
            sites.insert(at, site);
        }
    }

    /// One site per key, the way the base-game map is kept: a primary-language
    /// site replaces whatever is there, another language only fills a gap.
    pub(crate) fn insert_single(
        &mut self,
        key: Arc<str>,
        uri: &Arc<str>,
        line0: u32,
        primary_lang: bool,
    ) {
        let site = self.site(uri, line0, false);
        let files = &mut self.files;
        let sites = self.by_key.entry(key).or_default();
        if primary_lang || sites.is_empty() {
            for dropped in sites.drain(..) {
                let file = &mut files[dropped.file as usize];
                file.sites -= 1;
                if file.uri.is_none() {
                    self.dead_sites -= 1;
                }
                self.total_sites -= 1;
            }
            sites.push(site);
        } else {
            files[site.file as usize].sites -= 1;
            self.total_sites -= 1;
        }
        self.maybe_sweep();
    }

    /// A base-game site, kept behind every workspace site of the key so it is
    /// the goto target only while the workspace defines nothing.
    pub(crate) fn insert_fallback(&mut self, key: &Arc<str>, uri: &Arc<str>, line0: u32) {
        let site = self.site(uri, line0, true);
        self.by_key.entry(Arc::clone(key)).or_default().push(site);
    }

    /// The goto target for `key`.
    pub(crate) fn get(&self, key: &str) -> Option<(&Arc<str>, u32)> {
        let site = self.by_key.get(key)?.iter().find(|site| self.live(site))?;
        self.files[site.file as usize]
            .uri
            .as_ref()
            .map(|uri| (uri, site.line))
    }

    /// Every workspace definition of `key`, goto target first; base-game
    /// fallbacks are not definitions the workspace can list or edit.
    pub(crate) fn workspace_sites(&self, key: &str) -> impl Iterator<Item = (&Arc<str>, u32)> {
        self.by_key
            .get(key)
            .into_iter()
            .flatten()
            .filter_map(move |site| {
                let file = &self.files[site.file as usize];
                file.uri
                    .as_ref()
                    .filter(|_| !file.vanilla)
                    .map(|uri| (uri, site.line))
            })
    }

    /// The goto target of every key, for the workspace symbol search.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Arc<str>, (&Arc<str>, u32))> {
        self.by_key.iter().filter_map(|(key, sites)| {
            let site = sites.iter().find(|site| self.live(site))?;
            self.files[site.file as usize]
                .uri
                .as_ref()
                .map(|uri| (key, (uri, site.line)))
        })
    }

    /// Drops every site in `uri`, the way a watched change or a close re-reads
    /// that one file before inserting its sites again. The file id is
    /// tombstoned, so the call is O(1) and a re-insert of the same uri gets a
    /// different id until its old sites are swept; the map is only swept
    /// once the dead sites add up, so a watched batch of a hundred files pays
    /// for one sweep, not a hundred.
    pub(crate) fn remove_file(&mut self, uri: &str) {
        let Some(id) = self.file_ids.remove(uri) else {
            return;
        };
        let file = &mut self.files[id as usize];
        file.uri = None;
        self.dead_sites += file.sites;
        if file.sites == 0 {
            self.free.push(id);
        } else {
            self.retired.push(id);
        }
        self.maybe_sweep();
    }

    fn maybe_sweep(&mut self) {
        // insert_single can drop tombstoned sites before a sweep. Count the
        // retired records too so those zero-site tombstones cannot accumulate.
        let dead = u64::from(self.dead_sites).max(self.retired.len() as u64);
        if dead * u64::from(SWEEP_DEAD_SHARE) > u64::from(self.total_sites) {
            self.sweep();
        }
    }

    /// Drops the sites of every removed file and the keys left without one.
    fn sweep(&mut self) {
        let files = &self.files;
        self.by_key.retain(|_, sites| {
            sites.retain(|site| files[site.file as usize].uri.is_some());
            !sites.is_empty()
        });
        self.total_sites -= self.dead_sites;
        self.dead_sites = 0;
        // No surviving Site refers to these IDs. Readers cannot observe the
        // transition: all mutations require &mut self under the caller's lock.
        for id in self.retired.drain(..) {
            self.files[id as usize].sites = 0;
            self.free.push(id);
        }
    }

    /// Keys with a live site.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.iter().count()
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Keys in the map, tombstoned ones included.
    #[cfg(test)]
    fn raw_len(&self) -> usize {
        self.by_key.len()
    }
}

impl FromIterator<(Arc<str>, (Arc<str>, u32))> for LocLocations {
    fn from_iter<I: IntoIterator<Item = (Arc<str>, (Arc<str>, u32))>>(iter: I) -> Self {
        let mut out = Self::default();
        for (key, (uri, line0)) in iter {
            out.insert(key, &uri, line0, true);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    fn sites(map: &LocLocations, key: &str) -> Vec<(String, u32)> {
        map.workspace_sites(key)
            .map(|(uri, line)| (uri.to_string(), line))
            .collect()
    }

    // Structural storage measurements, not allocator usage or process RSS.
    fn storage(map: &LocLocations, label: &str) {
        eprintln!(
            "{label}: files={} capacity={} file_bytes={} live_files={} total_sites={} dead_sites={}",
            map.files.len(),
            map.files.capacity(),
            map.files.capacity() * std::mem::size_of::<File>(),
            map.file_ids.len(),
            map.total_sites,
            map.dead_sites,
        );
        eprintln!(
            "retired_capacity={} free_capacity={} queue_bytes={}",
            map.retired.capacity(),
            map.free.capacity(),
            (map.retired.capacity() + map.free.capacity()) * std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn churn_reclaims_file_records() {
        let mut map = LocLocations::default();
        let uri = a("file:///churn.yml");
        let key = a("churn");
        for _ in 0..100_000 {
            map.insert(Arc::clone(&key), &uri, 0, true);
            map.remove_file(&uri);
        }
        storage(&map, "empty churn");
        eprintln!("URI strong references={}", Arc::strong_count(&uri));
        assert_eq!(map.len(), 0);
        assert!(map.files.len() <= 1);
        assert!(map.files.capacity() <= 4);
        assert_eq!(Arc::strong_count(&uri), 1);
        assert_storage_invariants(&map);
    }

    #[test]
    fn mostly_live_churn_keeps_storage_bounded_and_sweeps_amortized() {
        let mut map = LocLocations::default();
        for i in 0..10_000 {
            map.insert(
                a(&format!("k{i}")),
                &a(&format!("file:///live{i}.yml")),
                i,
                true,
            );
        }
        let uri = a("file:///churn.yml");
        let key = a("churn");
        let mut sweeps = 0;
        for _ in 0..100_000 {
            map.insert(Arc::clone(&key), &uri, 0, true);
            map.remove_file(&uri);
            if map.dead_sites == 0 {
                sweeps += 1;
            }
        }
        storage(&map, "mostly live churn");
        eprintln!(
            "sweeps={sweeps}; URI strong references={}",
            Arc::strong_count(&uri)
        );
        assert_eq!(map.len(), 10_000);
        assert_eq!(map.get("k9999"), Some((&a("file:///live9999.yml"), 9999)));
        assert_eq!(sweeps, 69);
        assert!(map.files.len() < 12_000);
        assert!(map.files.capacity() < 24_000);
        assert_storage_invariants(&map);
    }

    fn assert_storage_invariants(map: &LocLocations) {
        let mut counts = vec![0; map.files.len()];
        for site in map.by_key.values().flatten() {
            counts[site.file as usize] += 1;
        }
        let mut dead = 0;
        for (id, file) in map.files.iter().enumerate() {
            assert_eq!(file.sites, counts[id]);
            if let Some(uri) = &file.uri {
                assert_eq!(map.file_ids.get(uri), Some(&(id as u32)));
                assert!(!map.free.contains(&(id as u32)));
                assert!(!map.retired.contains(&(id as u32)));
            } else {
                dead += counts[id];
                assert_ne!(
                    map.free.contains(&(id as u32)),
                    map.retired.contains(&(id as u32))
                );
            }
        }
        for &id in &map.free {
            assert_eq!(counts[id as usize], 0);
        }
        let mut free = map.free.clone();
        free.sort_unstable();
        free.dedup();
        assert_eq!(free.len(), map.free.len());
        assert_eq!(map.total_sites, counts.iter().sum::<u32>());
        assert_eq!(map.dead_sites, dead);
    }

    #[test]
    fn retired_ids_wait_for_sweep_and_reuse_resets_provenance() {
        let mut map = LocLocations::default();
        let big = a("file:///big.yml");
        for i in 0..100 {
            map.insert(a(&format!("k{i}")), &big, i, true);
        }
        let old = a("file:///old.yml");
        let weak = Arc::downgrade(&old);
        map.insert_fallback(&a("gone"), &old, 1);
        let retired = map.file_ids[&old];
        map.remove_file(&old);
        drop(old);
        assert!(weak.upgrade().is_none());
        assert_eq!(map.retired, vec![retired]);
        let fresh = a("file:///fresh.yml");
        map.insert(a("new"), &fresh, 2, true);
        assert_ne!(map.file_ids[&fresh], retired);
        assert_eq!(map.get("gone"), None);
        assert_storage_invariants(&map);

        map.sweep();
        let reused = a("file:///reused.yml");
        map.insert(a("new"), &reused, 3, false);
        assert_eq!(map.file_ids[&reused], retired);
        assert_eq!(
            sites(&map, "new"),
            vec![(fresh.to_string(), 2), (reused.to_string(), 3)]
        );
        assert_eq!(map.get("gone"), None);
        map.remove_file(&reused);
        map.sweep();
        let vanilla = a("file:///vanilla.yml");
        map.insert_fallback(&a("new"), &vanilla, 4);
        assert_eq!(map.file_ids[&vanilla], retired);
        map.insert(a("new"), &big, 5, false);
        assert_eq!(
            sites(&map, "new"),
            vec![(fresh.to_string(), 2), (big.to_string(), 5)]
        );
        map.remove_file(&fresh);
        assert_eq!(map.get("new"), Some((&big, 5)));
        map.remove_file(&big);
        assert_eq!(map.get("new"), Some((&vanilla, 4)));
        assert!(sites(&map, "new").is_empty());
        assert_storage_invariants(&map);
    }

    #[test]
    fn zero_site_files_are_reusable_without_sweeping() {
        let mut map = LocLocations::default();
        map.insert_single(a("k"), &a("file:///winner.yml"), 1, true);
        let rejected = a("file:///rejected.yml");
        for _ in 0..1_000 {
            map.insert_single(a("k"), &rejected, 2, false);
            map.remove_file(&rejected);
            assert_eq!(map.total_sites, 1);
            assert!(map.retired.is_empty());
        }
        assert_eq!(map.files.len(), 2);
        assert_storage_invariants(&map);
    }

    #[test]
    fn single_replacements_keep_tombstone_counters_and_storage_bounded() {
        let mut map = LocLocations::default();
        let big = a("file:///big.yml");
        for i in 0..100 {
            map.insert(a(&format!("k{i}")), &big, i, true);
        }
        let uri = a("file:///churn.yml");
        map.insert_single(a("churn"), &uri, 0, true);
        for i in 0..1_000 {
            map.remove_file(&uri);
            map.insert_single(a("churn"), &uri, i, true);
            assert_storage_invariants(&map);
        }
        assert!(map.files.len() < 20);
        map.remove_file(&big);
        assert_eq!(map.get("churn"), Some((&uri, 999)));
        assert_storage_invariants(&map);
    }

    #[test]
    fn last_primary_language_site_is_the_goto_target() {
        let mut map = LocLocations::default();
        map.insert(a("k"), &a("file:///fr.yml"), 3, false);
        map.insert(a("k"), &a("file:///en_a.yml"), 5, true);
        map.insert(a("k"), &a("file:///de.yml"), 7, false);
        map.insert(a("k"), &a("file:///en_b.yml"), 9, true);
        assert_eq!(map.get("k"), Some((&a("file:///en_b.yml"), 9)));
        assert_eq!(
            sites(&map, "k"),
            vec![
                ("file:///en_b.yml".to_string(), 9),
                ("file:///en_a.yml".to_string(), 5),
                ("file:///fr.yml".to_string(), 3),
                ("file:///de.yml".to_string(), 7),
            ]
        );
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("k"));
        assert!(!map.contains_key("other"));
    }

    #[test]
    fn single_mode_keeps_one_site_per_key() {
        let mut map = LocLocations::default();
        map.insert_single(a("k"), &a("file:///fr.yml"), 3, false);
        map.insert_single(a("k"), &a("file:///de.yml"), 4, false);
        assert_eq!(map.get("k"), Some((&a("file:///fr.yml"), 3)));
        map.insert_single(a("k"), &a("file:///en.yml"), 5, true);
        assert_eq!(map.get("k"), Some((&a("file:///en.yml"), 5)));
        assert_eq!(sites(&map, "k").len(), 1);
    }

    #[test]
    fn vanilla_fallback_is_neither_listed_nor_a_workspace_site() {
        let mut map = LocLocations::default();
        map.insert_fallback(&a("only_vanilla"), &a("file:///vanilla.yml"), 1);
        map.insert(a("both"), &a("file:///mod.yml"), 2, false);
        map.insert_fallback(&a("both"), &a("file:///vanilla.yml"), 8);
        assert_eq!(
            map.get("only_vanilla"),
            Some((&a("file:///vanilla.yml"), 1))
        );
        assert!(sites(&map, "only_vanilla").is_empty());
        assert_eq!(map.get("both"), Some((&a("file:///mod.yml"), 2)));
        assert_eq!(
            sites(&map, "both"),
            vec![("file:///mod.yml".to_string(), 2)]
        );
        // A later workspace site still goes ahead of the fallback.
        map.insert_fallback(&a("late"), &a("file:///vanilla.yml"), 1);
        map.insert(a("late"), &a("file:///fr.yml"), 4, false);
        assert_eq!(map.get("late"), Some((&a("file:///fr.yml"), 4)));
        assert_eq!(sites(&map, "late"), vec![("file:///fr.yml".to_string(), 4)]);
    }

    #[test]
    fn vanilla_fallback_takes_over_when_the_workspace_definition_goes() {
        let mut map = LocLocations::default();
        map.insert(a("both"), &a("file:///mod.yml"), 2, true);
        map.insert_fallback(&a("both"), &a("file:///vanilla.yml"), 8);
        map.remove_file("file:///mod.yml");
        assert_eq!(map.get("both"), Some((&a("file:///vanilla.yml"), 8)));
        assert!(sites(&map, "both").is_empty());
        assert_eq!(
            map.iter()
                .map(|(k, (uri, line))| (k.to_string(), uri.to_string(), line))
                .collect::<Vec<_>>(),
            vec![("both".to_string(), "file:///vanilla.yml".to_string(), 8)]
        );
        // And a re-added workspace definition goes back in front of it.
        map.insert(a("both"), &a("file:///mod.yml"), 12, true);
        assert_eq!(map.get("both"), Some((&a("file:///mod.yml"), 12)));
    }

    #[test]
    fn remove_file_touches_only_that_file() {
        let mut map = LocLocations::default();
        let en = a("file:///en.yml");
        let fr = a("file:///fr.yml");
        map.insert(a("k"), &en, 1, true);
        map.insert(a("k"), &fr, 1, false);
        map.insert(a("gone"), &en, 2, true);
        map.remove_file(&en);
        map.insert(a("k"), &en, 10, true);
        map.insert(a("fresh"), &en, 11, true);
        assert_eq!(
            sites(&map, "k"),
            vec![
                ("file:///en.yml".to_string(), 10),
                ("file:///fr.yml".to_string(), 1)
            ]
        );
        assert!(!map.contains_key("gone"));
        assert_eq!(map.get("fresh"), Some((&en, 11)));
        map.remove_file(&fr);
        assert_eq!(sites(&map, "k"), vec![("file:///en.yml".to_string(), 10)]);
        map.remove_file("file:///never-seen.yml");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn removed_sites_are_invisible_before_the_sweep_and_gone_after() {
        let mut map = LocLocations::default();
        let big = a("file:///big.yml");
        for i in 0..100 {
            map.insert(a(&format!("k{i}")), &big, i, true);
        }
        let small = a("file:///small.yml");
        map.insert(a("s"), &small, 0, true);
        map.insert(a("k0"), &small, 1, false);
        // One site of a hundred: tombstoned, not swept.
        map.remove_file(&small);
        assert_eq!(map.raw_len(), 101);
        assert_eq!(map.len(), 100);
        assert!(!map.contains_key("s"));
        assert!(map.iter().all(|(k, _)| k.as_ref() != "s"));
        assert_eq!(sites(&map, "k0"), vec![("file:///big.yml".to_string(), 0)]);
        // Re-inserting the removed uri records only the fresh sites.
        map.insert(a("s"), &small, 5, true);
        assert_eq!(sites(&map, "s"), vec![("file:///small.yml".to_string(), 5)]);
        assert_eq!(sites(&map, "k0"), vec![("file:///big.yml".to_string(), 0)]);
        // Most of the map dead: swept, the ghosts and their keys dropped.
        map.remove_file(&big);
        assert_eq!(map.raw_len(), 1);
        assert_eq!(map.len(), 1);
        assert_eq!(map.dead_sites, 0);
        assert_eq!(map.total_sites, 1);
        assert_eq!(map.get("s"), Some((&small, 5)));
    }

    #[test]
    fn iter_yields_the_goto_target_per_key() {
        let map: LocLocations = [
            (a("a"), (a("file:///x.yml"), 1)),
            (a("b"), (a("file:///y.yml"), 2)),
        ]
        .into_iter()
        .collect();
        let mut seen: Vec<(String, String, u32)> = map
            .iter()
            .map(|(k, (uri, line))| (k.to_string(), uri.to_string(), line))
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("a".to_string(), "file:///x.yml".to_string(), 1),
                ("b".to_string(), "file:///y.yml".to_string(), 2),
            ]
        );
        assert_eq!(map.len(), 2);
    }
}
