use crate::commands::{Lang, LocEntry};
use crate::service::LocService;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

pub type LocKey = Arc<str>;
pub type LocKeySet = FxHashSet<LocKey>;

#[derive(Debug, Clone, Default)]
pub struct LocIndex {
    per_language: FxHashMap<Lang, LocKeySet>,
    union: LocKeySet,
    languages_with_data: Vec<Lang>,
    entries: FxHashMap<LocKey, LocEntry>,
}

impl LocIndex {
    pub fn build(service: &LocService) -> Self {
        Self::build_scoped(service, None)
    }

    pub fn build_scoped(service: &LocService, langs: Option<&[Lang]>) -> Self {
        let mut per_language: FxHashMap<Lang, LocKeySet> = FxHashMap::default();
        let mut union = LocKeySet::default();
        let mut entries: FxHashMap<LocKey, LocEntry> = FxHashMap::default();

        for file in service.files() {
            let Some(lang) = file.lang else { continue };
            let set = per_language.entry(lang).or_default();
            for entry in &file.entries {
                let lower = Self::intern_key(&mut union, entry.key.to_lowercase());
                set.insert(Arc::clone(&lower));
            }
        }
        let english = per_language.get(&Lang::English);
        for file in service.files() {
            let Some(lang) = file.lang else { continue };
            for entry in &file.entries {
                if entry.commands.is_empty() && entry.jomini_commands.is_empty() {
                    continue;
                }
                let Some(lower) = union.get(entry.key.to_lowercase().as_str()).cloned() else {
                    continue;
                };
                if lang != Lang::English && english.is_some_and(|s| s.contains(&lower)) {
                    continue;
                }
                match entries.get(&lower) {
                    Some(_) if lang != Lang::English => {}
                    _ => {
                        entries.insert(lower, entry.clone());
                    }
                }
            }
        }

        let mut languages_with_data = service.languages();
        if let Some(set) = langs {
            languages_with_data.retain(|l| set.contains(l));
        }
        Self {
            per_language,
            union,
            languages_with_data,
            entries,
        }
    }

    fn intern_key(union: &mut LocKeySet, key: String) -> LocKey {
        if let Some(existing) = union.get(key.as_str()) {
            Arc::clone(existing)
        } else {
            let key: LocKey = Arc::from(key);
            union.insert(Arc::clone(&key));
            key
        }
    }

    pub fn merge_cached_keys(
        &mut self,
        per_language: Vec<(Lang, Vec<String>)>,
        langs: Option<&[Lang]>,
    ) {
        for (lang, keys) in per_language {
            let set = self.per_language.entry(lang).or_default();
            for key in keys {
                let key = Self::intern_key(&mut self.union, key);
                set.insert(key);
            }
            let allowed = langs.map(|ls| ls.contains(&lang)).unwrap_or(true);
            if allowed && !self.languages_with_data.contains(&lang) {
                self.languages_with_data.push(lang);
            }
        }
    }

    pub fn merge_from(&mut self, other: &LocIndex, langs: Option<&[Lang]>) {
        for (key, entry) in &other.entries {
            if self
                .per_language
                .get(&Lang::English)
                .is_some_and(|s| s.contains(key))
            {
                continue;
            }
            self.entries
                .entry(Arc::clone(key))
                .or_insert_with(|| entry.clone());
        }
        for lang in &other.languages_with_data {
            let Some(keys) = other.per_language.get(lang) else {
                continue;
            };
            let set = self.per_language.entry(*lang).or_default();
            for key in keys {
                let key = match self.union.get(&**key) {
                    Some(existing) => Arc::clone(existing),
                    None => {
                        self.union.insert(Arc::clone(key));
                        Arc::clone(key)
                    }
                };
                set.insert(key);
            }
            let allowed = langs.map(|ls| ls.contains(lang)).unwrap_or(true);
            if allowed && !self.languages_with_data.contains(lang) {
                self.languages_with_data.push(*lang);
            }
        }
    }

    pub fn exists_any(&self, key_lower: &str) -> bool {
        self.union.contains(key_lower)
    }

    pub fn missing_synced_languages(&self, key_lower: &str) -> Vec<Lang> {
        self.languages_with_data
            .iter()
            .copied()
            .filter(|lang| {
                self.per_language
                    .get(lang)
                    .map(|set| !set.contains(key_lower))
                    .unwrap_or(true)
            })
            .collect()
    }

    pub fn entry(&self, key_lower: &str) -> Option<&LocEntry> {
        self.entries.get(key_lower)
    }

    pub fn languages_with_data(&self) -> &[Lang] {
        &self.languages_with_data
    }

    pub fn union(&self) -> &LocKeySet {
        &self.union
    }

    pub fn key(&self, key: &str) -> Option<LocKey> {
        self.union.get(key.to_lowercase().as_str()).cloned()
    }
}

pub fn per_language_keys(service: &LocService) -> Vec<(String, Vec<String>)> {
    let mut per: FxHashMap<Lang, LocKeySet> = FxHashMap::default();
    for file in service.files() {
        let Some(lang) = file.lang else { continue };
        let set = per.entry(lang).or_default();
        for entry in &file.entries {
            set.insert(entry.key.to_lowercase().into());
        }
    }
    per.into_iter()
        .map(|(lang, keys)| {
            (
                lang.to_string(),
                keys.into_iter().map(|key| key.to_string()).collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::LocService;

    fn service_from(files: &[(&str, &str)]) -> LocService {
        LocService::from_files(
            files
                .iter()
                .map(|(p, t)| (p.to_string(), t.to_string()))
                .collect(),
        )
    }

    #[test]
    fn exists_any_is_case_insensitive() {
        let svc = service_from(&[("a_l_english.yml", "l_english:\n MY_Key: \"hi\"\n")]);
        let idx = LocIndex::build(&svc);
        assert!(idx.exists_any("my_key"));
        assert!(!idx.exists_any("absent"));
    }

    #[test]
    fn indexed_keys_share_one_allocation() {
        let svc = service_from(&[
            ("a_l_english.yml", "l_english:\n shared_key: \"a\"\n"),
            ("a_l_german.yml", "l_german:\n shared_key: \"b\"\n"),
        ]);
        let idx = LocIndex::build(&svc);
        let union = idx.union.get("shared_key").unwrap();
        let english = idx
            .per_language
            .get(&Lang::English)
            .unwrap()
            .get("shared_key")
            .unwrap();
        let german = idx
            .per_language
            .get(&Lang::German)
            .unwrap()
            .get("shared_key")
            .unwrap();
        assert!(Arc::ptr_eq(union, english));
        assert!(Arc::ptr_eq(union, german));
    }

    #[test]
    fn synced_only_flags_languages_with_data() {
        let svc = service_from(&[
            (
                "a_l_english.yml",
                "l_english:\n key_a: \"a\"\n key_b: \"b\"\n",
            ),
            ("a_l_german.yml", "l_german:\n key_a: \"a\"\n"),
        ]);
        let idx = LocIndex::build(&svc);
        assert!(idx.missing_synced_languages("key_a").is_empty());
        let missing = idx.missing_synced_languages("key_b");
        assert_eq!(missing, vec![Lang::German]);
        assert!(!missing.contains(&Lang::French));
    }

    #[test]
    fn merge_from_folds_a_second_index_under_this_one() {
        let workspace = LocIndex::build(&service_from(&[(
            "ws_l_english.yml",
            "l_english:\n shared: \"mod [ROOT.GetName]\"\n ws_only: \"w\"\n",
        )]));
        let vanilla = LocIndex::build(&service_from(&[
            (
                "v_l_english.yml",
                "l_english:\n shared: \"base [ROOT.GetName]\"\n base_only: \"base [ROOT.GetFlag]\"\n",
            ),
            ("v_l_german.yml", "l_german:\n base_only: \"b\"\n"),
        ]));
        let mut merged = workspace;
        merged.merge_from(&vanilla, None);

        assert!(merged.exists_any("ws_only"));
        assert!(merged.exists_any("base_only"));
        assert_eq!(
            merged.languages_with_data(),
            &[Lang::English, Lang::German],
            "base-game-only languages must join, in the order the merge saw them"
        );
        assert_eq!(
            merged.missing_synced_languages("ws_only"),
            vec![Lang::German]
        );
        assert!(merged.entry("shared").unwrap().desc.contains("mod"));
        assert!(merged.entry("base_only").is_some());
        assert!(Arc::ptr_eq(
            merged.union.get("base_only").unwrap(),
            vanilla.union.get("base_only").unwrap()
        ));
    }

    #[test]
    fn merge_from_scopes_languages_like_the_cached_merge() {
        let mut english_only = LocIndex::build_scoped(
            &service_from(&[("ws_l_english.yml", "l_english:\n ws_only: \"w\"\n")]),
            Some(&[Lang::English]),
        );
        let vanilla = LocIndex::build(&service_from(&[(
            "v_l_german.yml",
            "l_german:\n base_only: \"b\"\n",
        )]));
        english_only.merge_from(&vanilla, Some(&[Lang::English]));
        assert_eq!(english_only.languages_with_data(), &[Lang::English]);
        assert!(english_only.exists_any("base_only"));
    }

    #[test]
    fn build_scoped_restricts_missing_check_to_chosen_languages() {
        let svc = service_from(&[
            (
                "a_l_english.yml",
                "l_english:\n key_a: \"a\"\n key_b: \"b\"\n",
            ),
            ("a_l_german.yml", "l_german:\n key_a: \"a\"\n"),
        ]);
        let idx = LocIndex::build_scoped(&svc, Some(&[Lang::English]));
        assert!(idx.missing_synced_languages("key_b").is_empty());
        assert_eq!(idx.languages_with_data(), &[Lang::English]);
        assert!(idx.exists_any("key_a"));
    }

    #[test]
    fn english_without_commands_blocks_other_languages() {
        let braz = ("b_l_braz_por.yml", "l_braz_por:\n shared: \"[Grecia]\"\n");
        let eng = ("a_l_english.yml", "l_english:\n shared: \"plain\"\n");
        for files in [[braz, eng], [eng, braz]] {
            let idx = LocIndex::build(&service_from(&files));
            assert!(idx.exists_any("shared"));
            assert!(
                idx.entry("shared").is_none(),
                "English without commands must not keep another language's, order={files:?}"
            );
        }
    }

    #[test]
    fn english_with_commands_wins_over_other_languages() {
        let braz = ("b_l_braz_por.yml", "l_braz_por:\n shared: \"[Grecia]\"\n");
        let eng = (
            "a_l_english.yml",
            "l_english:\n shared: \"[ROOT.GetName]\"\n",
        );
        for files in [[braz, eng], [eng, braz]] {
            let desc = &idx_entry(&service_from(&files)).desc;
            assert!(
                desc.contains("ROOT.GetName"),
                "expected English commands, got {desc:?}, order={files:?}"
            );
        }
    }

    fn idx_entry(svc: &LocService) -> LocEntry {
        LocIndex::build(svc)
            .entry("shared")
            .cloned()
            .expect("command-bearing representative")
    }

    #[test]
    fn no_english_keeps_first_command_bearing() {
        let braz = ("b_l_braz_por.yml", "l_braz_por:\n shared: \"[Grecia]\"\n");
        let pol = (
            "c_l_polish.yml",
            "l_polish:\n shared: \"[GRE.GetNameWithFlag]\"\n",
        );
        assert!(
            idx_entry(&service_from(&[braz, pol]))
                .desc
                .contains("Grecia")
        );
        assert!(
            idx_entry(&service_from(&[pol, braz]))
                .desc
                .contains("GetNameWithFlag")
        );
    }

    #[test]
    fn merge_from_does_not_adopt_vanilla_commands_when_workspace_has_english() {
        let workspace = LocIndex::build(&service_from(&[(
            "ws_l_english.yml",
            "l_english:\n shared: \"plain\"\n",
        )]));
        let vanilla = LocIndex::build(&service_from(&[(
            "v_l_german.yml",
            "l_german:\n shared: \"[GetName]\"\n",
        )]));
        let mut merged = workspace;
        merged.merge_from(&vanilla, None);
        assert!(merged.exists_any("shared"));
        assert!(
            merged.entry("shared").is_none(),
            "workspace English without commands must not pick up vanilla's"
        );
    }

    #[test]
    fn clone_snapshot_is_independent() {
        let idx = LocIndex::build(&service_from(&[(
            "a_l_english.yml",
            "l_english:\n a_key: \"hi\"\n",
        )]));
        let snap = idx.clone();
        let other = LocIndex::build(&service_from(&[(
            "b_l_english.yml",
            "l_english:\n b_key: \"hi\"\n",
        )]));
        let mut mutated = snap.clone();
        mutated.merge_from(&other, None);
        assert!(mutated.exists_any("b_key"));
        assert!(
            !snap.exists_any("b_key"),
            "snapshot must not see merged key"
        );
        assert!(!idx.exists_any("b_key"), "original must not see merged key");
        assert!(snap.exists_any("a_key"));
    }
}
