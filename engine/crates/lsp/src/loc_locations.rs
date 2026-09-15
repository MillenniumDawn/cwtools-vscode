//! Where every loc key is defined: the goto target per key, and behind it
//! every other definition site the loc scan parsed, so find-references can
//! list the definition half without walking the localisation tree (#474).
//!
//! A site is `(file id, line0)`, eight bytes; keys are the `LocIndex`'s
//! interned `Arc<str>`s, so the map adds no string memory of its own. Sites
//! are kept per key with the goto target first: the last primary-language
//! file scanned wins that slot, everything else keeps scan order, and a
//! base-game site is only ever the fallback for a key the workspace does not
//! define (the winners the one-per-key map had before this).

use std::sync::Arc;

use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Site {
    file: u32,
    line: u32,
}

#[derive(Debug)]
struct File {
    uri: Arc<str>,
    vanilla: bool,
}

#[derive(Default, Debug)]
pub(crate) struct LocLocations {
    files: Vec<File>,
    file_ids: FxHashMap<Arc<str>, u32>,
    by_key: FxHashMap<Arc<str>, Vec<Site>>,
}

impl LocLocations {
    fn file_id(&mut self, uri: &Arc<str>, vanilla: bool) -> u32 {
        if let Some(&id) = self.file_ids.get(uri) {
            return id;
        }
        let id = self.files.len() as u32;
        self.files.push(File {
            uri: Arc::clone(uri),
            vanilla,
        });
        self.file_ids.insert(Arc::clone(uri), id);
        id
    }

    /// Records a workspace site. A primary-language site becomes the goto
    /// target (the last one recorded wins); any other language appends behind
    /// the sites already there, but ahead of a base-game fallback.
    pub(crate) fn insert(&mut self, key: Arc<str>, uri: &Arc<str>, line0: u32, primary_lang: bool) {
        let site = Site {
            file: self.file_id(uri, false),
            line: line0,
        };
        let sites = self.by_key.entry(key).or_default();
        if primary_lang {
            sites.insert(0, site);
        } else {
            let at = sites
                .iter()
                .position(|s| self.files[s.file as usize].vanilla)
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
        let site = Site {
            file: self.file_id(uri, false),
            line: line0,
        };
        let sites = self.by_key.entry(key).or_default();
        if primary_lang || sites.is_empty() {
            sites.clear();
            sites.push(site);
        }
    }

    /// A base-game site, taken only for a key the workspace does not define.
    pub(crate) fn insert_fallback(&mut self, key: &Arc<str>, uri: &Arc<str>, line0: u32) {
        if self.by_key.contains_key(key) {
            return;
        }
        let site = Site {
            file: self.file_id(uri, true),
            line: line0,
        };
        self.by_key.insert(Arc::clone(key), vec![site]);
    }

    /// The goto target for `key`.
    pub(crate) fn get(&self, key: &str) -> Option<(&Arc<str>, u32)> {
        let site = self.by_key.get(key)?.first()?;
        Some((&self.files[site.file as usize].uri, site.line))
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
                (!file.vanilla).then_some((&file.uri, site.line))
            })
    }

    /// The goto target of every key, for the workspace symbol search.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Arc<str>, (&Arc<str>, u32))> {
        self.by_key.iter().filter_map(|(key, sites)| {
            let site = sites.first()?;
            Some((key, (&self.files[site.file as usize].uri, site.line)))
        })
    }

    /// Drops every site in `uri`, the way a watched change or a close re-reads
    /// that one file before inserting its sites again. Sweeps the whole map: a
    /// file's keys are not kept per file, and the events this serves are rare
    /// enough that the sweep beats holding another 2M-entry list.
    pub(crate) fn remove_file(&mut self, uri: &str) {
        let Some(&id) = self.file_ids.get(uri) else {
            return;
        };
        self.by_key.retain(|_, sites| {
            sites.retain(|site| site.file != id);
            !sites.is_empty()
        });
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_key.len()
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.by_key.contains_key(key)
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
