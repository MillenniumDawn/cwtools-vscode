//! The display text behind every loc key, for hover, inlay titles and graph
//! labels, kept per file so an edit to one loc file can subtract exactly what
//! that file contributed (#476). Keys are the `LocIndex`'s interned `Arc<str>`s.
//!
//! Entries are kept per key in the order the scan first saw their files,
//! with the base game's behind every workspace one, so the first entry is
//! the mod's own text while the mod defines the key and the base game's once
//! it does not. A file keeps its rank across a removal, so an edit that
//! re-inserts it lands where the scan put it instead of behind the other
//! files' translations of the same key, which the inlay and graph readers
//! would show as a label flipping while typing. Removing a file tombstones
//! its id the way `LocLocations` does: readers skip entries of a dead file,
//! and the sweep that drops them runs once the dead entries are a noticeable
//! share of the map, not once per keystroke.

use std::sync::Arc;

use cwtools_localization::Lang;
use rustc_hash::FxHashMap;

/// Sweep when the dead entries exceed this share of all recorded entries.
const SWEEP_DEAD_SHARE: u32 = 8;

/// The one file id every base-game entry is recorded under; never removed.
const VANILLA_URI: &str = "<vanilla>";

#[derive(Debug)]
struct Entry {
    file: u32,
    lang: Lang,
    text: String,
}

#[derive(Debug)]
struct File {
    vanilla: bool,
    /// Where the file's entries sort among the other files' of a key: the
    /// order the file was first seen in, kept across removals.
    rank: u32,
    /// Removed; its entries are skipped by readers until the next sweep.
    dead: bool,
    /// Entries recorded for this file, so a removal knows what it tombstoned.
    entries: u32,
}

#[derive(Default, Debug)]
pub(crate) struct LocText {
    files: Vec<File>,
    file_ids: FxHashMap<Arc<str>, u32>,
    /// Never cleared: a removed uri that comes back gets its old rank.
    ranks: FxHashMap<Arc<str>, u32>,
    by_key: FxHashMap<Arc<str>, Vec<Entry>>,
    total_entries: u32,
    dead_entries: u32,
}

impl LocText {
    fn file_id(&mut self, uri: &str, vanilla: bool) -> u32 {
        if let Some(&id) = self.file_ids.get(uri) {
            return id;
        }
        let id = self.files.len() as u32;
        let uri: Arc<str> = Arc::from(uri);
        let next_rank = self.ranks.len() as u32;
        let rank = *self.ranks.entry(Arc::clone(&uri)).or_insert(next_rank);
        self.files.push(File {
            vanilla,
            rank,
            dead: false,
            entries: 0,
        });
        self.file_ids.insert(uri, id);
        id
    }

    fn entry(&mut self, uri: &str, vanilla: bool, lang: Lang, text: String) -> Entry {
        let file = self.file_id(uri, vanilla);
        self.files[file as usize].entries += 1;
        self.total_entries += 1;
        Entry { file, lang, text }
    }

    fn live(&self, entry: &Entry) -> bool {
        !self.files[entry.file as usize].dead
    }

    /// Records a workspace translation at its file's place among the key's
    /// entries: behind the files seen before it, ahead of the ones seen after
    /// and of any base-game one.
    pub(crate) fn insert(&mut self, key: Arc<str>, uri: &str, lang: Lang, text: String) {
        let entry = self.entry(uri, false, lang, text);
        let files = &self.files;
        let rank = files[entry.file as usize].rank;
        let entries = self.by_key.entry(key).or_default();
        let at = entries
            .iter()
            .position(|e| {
                let file = &files[e.file as usize];
                file.vanilla || file.rank > rank
            })
            .unwrap_or(entries.len());
        entries.insert(at, entry);
    }

    /// A base-game translation, kept behind every workspace one of the key.
    pub(crate) fn insert_fallback(&mut self, key: &Arc<str>, lang: Lang, text: &str) {
        let entry = self.entry(VANILLA_URI, true, lang, text.to_string());
        self.by_key.entry(Arc::clone(key)).or_default().push(entry);
    }

    /// The live translations of `key`, workspace ones first; `None` when the
    /// key has none left.
    pub(crate) fn get<'a>(
        &'a self,
        key: &str,
    ) -> Option<impl Iterator<Item = (Lang, &'a str)> + use<'a>> {
        let entries = self.by_key.get(key)?;
        entries.iter().any(|e| self.live(e)).then(|| {
            entries
                .iter()
                .filter(|e| self.live(e))
                .map(|e| (e.lang, e.text.as_str()))
        })
    }

    /// Every live translation, in no particular order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&Arc<str>, Lang, &str)> {
        self.by_key.iter().flat_map(|(key, entries)| {
            entries
                .iter()
                .filter(|e| self.live(e))
                .map(move |e| (key, e.lang, e.text.as_str()))
        })
    }

    /// Live translations.
    pub(crate) fn len(&self) -> usize {
        (self.total_entries - self.dead_entries) as usize
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops every translation `uri` contributed, the way an edit or a watched
    /// change re-reads that one file before inserting its entries again. The
    /// file id is tombstoned, so the call is O(1) and a re-insert of the same
    /// uri gets a fresh id; the map is only swept once the dead entries add up.
    pub(crate) fn remove_file(&mut self, uri: &str) {
        let Some(id) = self.file_ids.remove(uri) else {
            return;
        };
        let file = &mut self.files[id as usize];
        file.dead = true;
        self.dead_entries += file.entries;
        if self.dead_entries.saturating_mul(SWEEP_DEAD_SHARE) > self.total_entries {
            self.sweep();
        }
    }

    /// Drops the entries of every removed file and the keys left without one.
    fn sweep(&mut self) {
        let files = &self.files;
        self.by_key.retain(|_, entries| {
            entries.retain(|e| !files[e.file as usize].dead);
            !entries.is_empty()
        });
        self.total_entries -= self.dead_entries;
        self.dead_entries = 0;
    }

    /// Keys in the map, tombstoned ones included.
    #[cfg(test)]
    fn raw_len(&self) -> usize {
        self.by_key.len()
    }
}

#[cfg(test)]
impl FromIterator<(Arc<str>, Vec<(Lang, String)>)> for LocText {
    /// A map from `(key, translations)` pairs under one test file, for the
    /// readers' tests, which only care about what a key resolves to.
    fn from_iter<I: IntoIterator<Item = (Arc<str>, Vec<(Lang, String)>)>>(iter: I) -> Self {
        let mut out = Self::default();
        for (key, translations) in iter {
            for (lang, text) in translations {
                out.insert(Arc::clone(&key), "file:///test.yml", lang, text);
            }
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

    fn texts(map: &LocText, key: &str) -> Option<Vec<(Lang, String)>> {
        map.get(key)
            .map(|it| it.map(|(lang, text)| (lang, text.to_string())).collect())
    }

    #[test]
    fn remove_file_touches_only_that_file() {
        let mut map = LocText::default();
        map.insert(a("k"), "file:///en.yml", Lang::English, "Hello".into());
        map.insert(a("k"), "file:///fr.yml", Lang::French, "Bonjour".into());
        map.insert(a("gone"), "file:///en.yml", Lang::English, "Gone".into());
        map.remove_file("file:///en.yml");
        assert_eq!(
            texts(&map, "k"),
            Some(vec![(Lang::French, "Bonjour".to_string())])
        );
        assert!(map.get("gone").is_none());
        // Re-inserting the removed uri records only the fresh entries.
        map.insert(a("k"), "file:///en.yml", Lang::English, "Hi".into());
        map.insert(a("fresh"), "file:///en.yml", Lang::English, "Fresh".into());
        assert_eq!(
            texts(&map, "k"),
            Some(vec![
                (Lang::English, "Hi".to_string()),
                (Lang::French, "Bonjour".to_string())
            ])
        );
        assert_eq!(
            texts(&map, "fresh"),
            Some(vec![(Lang::English, "Fresh".to_string())])
        );
        assert!(map.get("gone").is_none());
        map.remove_file("file:///never-seen.yml");
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn vanilla_fallback_sits_behind_the_workspace_and_takes_over_when_it_goes() {
        let mut map = LocText::default();
        map.insert_fallback(&a("k"), Lang::English, "Vanilla");
        map.insert(a("k"), "file:///mod.yml", Lang::English, "Mod".into());
        assert_eq!(
            texts(&map, "k"),
            Some(vec![
                (Lang::English, "Mod".to_string()),
                (Lang::English, "Vanilla".to_string())
            ])
        );
        map.remove_file("file:///mod.yml");
        assert_eq!(
            texts(&map, "k"),
            Some(vec![(Lang::English, "Vanilla".to_string())])
        );
        // And a re-added workspace translation goes back in front of it.
        map.insert(a("k"), "file:///mod.yml", Lang::English, "Mod again".into());
        assert_eq!(map.get("k").unwrap().next().unwrap().1, "Mod again");
    }

    #[test]
    fn reinserted_file_keeps_its_place_among_the_others() {
        let mut map = LocText::default();
        for file in ["a", "b", "c"] {
            map.insert(
                a("k"),
                &format!("file:///{file}.yml"),
                Lang::English,
                file.into(),
            );
        }
        // An edit re-reads b.yml: its entry goes back between a's and c's,
        // not behind them, so the first entry does not change under the edit.
        map.remove_file("file:///b.yml");
        map.insert(a("k"), "file:///b.yml", Lang::English, "b2".into());
        let order = |map: &LocText| -> Vec<String> {
            map.get("k").unwrap().map(|(_, t)| t.to_string()).collect()
        };
        assert_eq!(order(&map), ["a", "b2", "c"]);
        // A file never seen before sorts after every known one, and the base
        // game after that.
        map.insert_fallback(&a("k"), Lang::English, "vanilla");
        map.insert(a("k"), "file:///z.yml", Lang::English, "z".into());
        assert_eq!(order(&map), ["a", "b2", "c", "z", "vanilla"]);
        // The rank survives a sweep too.
        map.remove_file("file:///a.yml");
        map.remove_file("file:///c.yml");
        assert_eq!(map.dead_entries, 0, "sweep expected");
        map.insert(a("k"), "file:///a.yml", Lang::English, "a2".into());
        map.insert(a("k"), "file:///c.yml", Lang::English, "c2".into());
        assert_eq!(order(&map), ["a2", "b2", "c2", "z", "vanilla"]);
    }

    #[test]
    fn removed_entries_are_invisible_before_the_sweep_and_gone_after() {
        let mut map = LocText::default();
        for i in 0..100 {
            map.insert(
                a(&format!("k{i}")),
                "file:///big.yml",
                Lang::English,
                i.to_string(),
            );
        }
        map.insert(a("s"), "file:///small.yml", Lang::English, "S".into());
        map.insert(a("k0"), "file:///small.yml", Lang::French, "Zero".into());
        assert_eq!(map.len(), 102);
        // Two entries of a hundred: tombstoned, not swept.
        map.remove_file("file:///small.yml");
        assert_eq!(map.raw_len(), 101);
        assert_eq!(map.len(), 100);
        assert!(map.get("s").is_none());
        assert!(map.entries().all(|(k, _, _)| k.as_ref() != "s"));
        assert_eq!(
            texts(&map, "k0"),
            Some(vec![(Lang::English, "0".to_string())])
        );
        // Most of the map dead: swept, the ghosts and their keys dropped.
        map.insert(a("s"), "file:///small.yml", Lang::English, "S2".into());
        map.remove_file("file:///big.yml");
        assert_eq!(map.raw_len(), 1);
        assert_eq!(map.len(), 1);
        assert_eq!(map.dead_entries, 0);
        assert_eq!(map.total_entries, 1);
        assert_eq!(
            texts(&map, "s"),
            Some(vec![(Lang::English, "S2".to_string())])
        );
        assert!(!map.is_empty());
        map.remove_file("file:///small.yml");
        assert!(map.is_empty());
        assert!(map.get("s").is_none());
    }
}
