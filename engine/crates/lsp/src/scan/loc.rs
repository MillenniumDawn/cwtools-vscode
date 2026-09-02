use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tower_lsp::lsp_types::*;

use cwtools_localization::Lang;

use crate::lines::DocLines;
use crate::paths::{loc_display_text, path_to_uri};
use crate::validate::{loc_diag_to_validation_error, validation_error_to_diagnostic};
use crate::{Backend, LocLocationMap, LocTextMap};

use super::{VanillaLoc, stat_signature_for};

fn localisation_paths(
    roots: &[std::path::PathBuf],
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: cwtools_driver::DiscoveryPolicy,
) -> Vec<std::path::PathBuf> {
    match cwtools_driver::discover_localisation_files(roots, ignore_files, ignore_dirs, policy) {
        Ok(discovery) => {
            for failure in discovery.failures {
                tracing::warn!(
                    path = %failure.path.display(),
                    error = %failure.error,
                    "localisation discovery skipped path"
                );
            }
            discovery
                .files
                .into_iter()
                .filter(|file| file.kind == cwtools_file_manager::FileKind::Localisation)
                .map(|file| file.path)
                .collect()
        }
        Err(error) => {
            tracing::warn!(error = %error, "localisation discovery failed");
            Vec::new()
        }
    }
}

pub(crate) fn collect_loc_display(
    service: &cwtools_localization::LocService,
    index: &cwtools_localization::LocIndex,
    primary_lang: Lang,
    hover_all: bool,
    text: &mut LocTextMap,
    locations: &mut LocLocationMap,
) {
    for file in service.files() {
        let lang = file.lang.unwrap_or(Lang::English);
        let lang_included = hover_all || lang == primary_lang;
        let file_uri: Arc<str> = path_to_uri(std::path::Path::new(&file.path)).into();
        for entry in &file.entries {
            let key = index
                .key(&entry.key)
                .unwrap_or_else(|| Arc::from(entry.key.to_lowercase()));
            let loc = || {
                (
                    Arc::clone(&file_uri),
                    (entry.position.line.saturating_sub(1)) as u32,
                )
            };
            if lang == primary_lang {
                locations.insert(Arc::clone(&key), loc());
            } else {
                locations.entry(Arc::clone(&key)).or_insert_with(loc);
            }
            if !lang_included {
                continue;
            }
            let display = loc_display_text(&entry.desc);
            if !display.is_empty() {
                text.entry(key)
                    .or_default()
                    .push((lang, display.to_string()));
            }
        }
    }
}

impl Backend {
    pub(crate) fn compute_loc_signature(&self, root_path: &std::path::Path) -> u64 {
        let (ignore_files, ignore_dirs) = {
            let config = self.state.config.read();
            (
                config.ignore_file_patterns.clone(),
                config.ignore_dir_patterns.clone(),
            )
        };
        let files = localisation_paths(
            &[root_path.to_path_buf()],
            &ignore_files,
            &ignore_dirs,
            cwtools_driver::DiscoveryPolicy::Workspace,
        );
        stat_signature_for(&files)
    }

    /// definition sites and the same keys (#89).
    fn vanilla_loc(
        &self,
        loc_languages: Option<&[Lang]>,
        primary_lang: Lang,
        hover_all: bool,
    ) -> Option<Arc<VanillaLoc>> {
        let Some(dir) = self.state.config.read().vanilla_dir.clone() else {
            *self.state.vanilla_loc.lock() = None;
            return None;
        };
        let key = (
            dir,
            loc_languages.map(<[_]>::to_vec),
            primary_lang,
            hover_all,
        );
        if let Some((cached_key, loc)) = self.state.vanilla_loc.lock().as_ref()
            && *cached_key == key
        {
            return Some(Arc::clone(loc));
        }
        let service = cwtools_localization::LocService::from_paths(
            localisation_paths(
                std::slice::from_ref(&key.0),
                &[],
                &[],
                cwtools_driver::DiscoveryPolicy::Vanilla,
            ),
            cwtools_file_manager::file_manager::ScanBudget::default(),
            key.1.as_deref(),
        );
        if service.files().is_empty() {
            tracing::warn!(dir = %key.0.display(), "base-game dir holds no localisation files");
            return None;
        }
        let built = Arc::new(VanillaLoc::build(&service, primary_lang, hover_all));
        tracing::info!(
            "[loc] indexed base-game loc: {} files, {} keys, {} hover entries (kept for the session)",
            service.files().len(),
            built.index.union().len(),
            built.text.len(),
        );
        *self.state.vanilla_loc.lock() = Some((key, Arc::clone(&built)));
        Some(built)
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn rebuild_and_publish_loc(&self, root_path: &std::path::Path) {
        // so hover shows translations for keys that exist only there (#51). The
        // one read per session, not one per scan (#89).
        let (loc_languages, ignore_files, ignore_dirs) = {
            let config = self.state.config.read();
            (
                config.loc_languages.clone(),
                config.ignore_file_patterns.clone(),
                config.ignore_dir_patterns.clone(),
            )
        };

        let hover_all = self
            .state
            .hover_show_all_languages
            .load(std::sync::atomic::Ordering::Relaxed);
        let primary_lang = loc_languages
            .as_deref()
            .and_then(|l| l.first().copied())
            .unwrap_or(cwtools_localization::Lang::English);
        let parsed_languages = if hover_all {
            None
        } else {
            loc_languages.as_deref()
        };

        let extra_valid_refs: HashSet<String> = {
            // Lock order: rules -> info_service.
            let modifier_keys = self.state.rules.read().modifier_keys.clone();
            let info = self.state.info_service.read();
            crate::validate::loc_extra_valid_refs(&modifier_keys, &info.type_index)
        };

        let (loc_index, mut by_file, loc_text_map, loc_loc_map, source_hashes) =
            tokio::task::block_in_place(|| {
                // workspace is walked again (#89).
                let vanilla = self.vanilla_loc(parsed_languages, primary_lang, hover_all);
                let cached_vanilla_loc = if vanilla.is_some() {
                    self.state.vanilla_loc_keys.lock().take()
                } else {
                    self.state.vanilla_loc_keys.lock().clone()
                };
                let service = cwtools_localization::LocService::from_paths(
                    localisation_paths(
                        &[root_path.to_path_buf()],
                        &ignore_files,
                        &ignore_dirs,
                        cwtools_driver::DiscoveryPolicy::Workspace,
                    ),
                    cwtools_file_manager::file_manager::ScanBudget::default(),
                    parsed_languages,
                );
                let mut idx = cwtools_localization::LocIndex::build_scoped(
                    &service,
                    loc_languages.as_deref(),
                );
                if let Some(vanilla) = &vanilla {
                    idx.merge_from(&vanilla.index, loc_languages.as_deref());
                }
                if let Some(cached) = cached_vanilla_loc {
                    let typed: Vec<(cwtools_localization::Lang, Vec<String>)> = cached
                        .into_iter()
                        .filter_map(|(name, ks)| {
                            cwtools_localization::Lang::from_name(&name).map(|l| (l, ks))
                        })
                        .collect();
                    idx.merge_cached_keys(typed, loc_languages.as_deref());
                }
                let mut by_file: HashMap<String, Vec<Diagnostic>> = HashMap::new();
                for d in cwtools_localization::validate_loc_project_with_union(
                    &service,
                    loc_languages.as_deref(),
                    idx.union(),
                    &extra_valid_refs,
                ) {
                    let ve = loc_diag_to_validation_error(&d);
                    by_file
                        .entry(d.file.clone())
                        .or_default()
                        .push(validation_error_to_diagnostic(&ve, &DocLines::none()));
                }
                let mut lt = LocTextMap::default();
                let mut ll = LocLocationMap::default();
                for file in service.files() {
                    by_file.entry(file.path.clone()).or_default();
                }
                collect_loc_display(&service, &idx, primary_lang, hover_all, &mut lt, &mut ll);
                let source_hashes = by_file
                    .iter()
                    .filter_map(|(file, diagnostics)| {
                        let has_fix = diagnostics
                            .iter()
                            .any(|d| !crate::code_action::fixable_span_edits(d).is_empty());
                        if !has_fix {
                            return None;
                        }
                        let (text, _) = cwtools_file_manager::file_manager::read_text_capped(
                            std::path::Path::new(file),
                            crate::access::MAX_URI_READ_BYTES,
                        )
                        .ok()?;
                        Some((file.clone(), cwtools_cache::workspace::content_hash(&text)))
                    })
                    .collect::<HashMap<_, _>>();
                if let Some(vanilla) = &vanilla {
                    for (key, translations) in &vanilla.text {
                        lt.entry(Arc::clone(key))
                            .or_default()
                            .extend(translations.iter().cloned());
                    }
                    for (key, loc) in &vanilla.locations {
                        ll.entry(Arc::clone(key)).or_insert_with(|| loc.clone());
                    }
                }
                (idx, by_file, lt, ll, source_hashes)
            });
        let loc_key_index = tokio::task::block_in_place(|| {
            Arc::new(crate::completion::LocKeyIndex::build(
                loc_index.union().iter().map(AsRef::as_ref),
            ))
        });
        *self.state.loc_index.write() = Some(Arc::new(loc_index));
        *self.state.loc_key_index.write() = Some(loc_key_index);
        *self.state.loc_text.write() = loc_text_map;
        *self.state.loc_locations.write() = loc_loc_map;

        let open_uris: HashSet<String> = self.state.documents.lock().keys().cloned().collect();
        let current_uris: HashSet<String> = by_file
            .keys()
            .map(|file| path_to_uri(std::path::Path::new(file)))
            .filter(|uri| !open_uris.contains(uri))
            .collect();
        let stale_uris = {
            let mut previous = self.state.published_loc_uris.lock();
            let stale = previous
                .difference(&current_uris)
                .cloned()
                .collect::<Vec<_>>();
            *previous = current_uris;
            stale
        };
        for uri in stale_uris {
            if let Ok(uri) = Url::parse(&uri) {
                self.publish_filtered(uri, Vec::new(), None, None).await;
            }
        }
        for (file, mut diags) in by_file.drain() {
            let uri = path_to_uri(std::path::Path::new(&file));
            if open_uris.contains(&uri) {
                continue;
            }
            if let Ok(uri_obj) = Url::parse(&uri) {
                let file_path = std::path::PathBuf::from(&file);
                let inline_ignored = tokio::task::spawn_blocking(move || {
                    cwtools_file_manager::file_manager::read_text_capped(
                        &file_path,
                        crate::access::MAX_URI_READ_BYTES,
                    )
                    .ok()
                    .map(|(text, _)| {
                        cwtools_validation::inline_ignore::extract_inline_ignored_codes(&text)
                    })
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
                crate::validate::drop_inline_suppressed(&mut diags, &inline_ignored);
                self.publish_filtered(uri_obj, diags, None, source_hashes.get(&file).copied())
                    .await;
            }
        }
        cwtools_profiling::log_rss("loc_rebuild_done");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_file_manager::file_manager::ScanBudget;
    use cwtools_localization::{Lang, LocIndex, LocService};

    #[test]
    fn collect_loc_display_respects_primary_and_hover_all() {
        let tmp = tempfile::tempdir().unwrap();
        let loc_dir = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc_dir).unwrap();
        std::fs::write(
            loc_dir.join("a_l_english.yml"),
            "l_english:\n my_key:0 \"Hello\"\n",
        )
        .unwrap();
        std::fs::write(
            loc_dir.join("a_l_french.yml"),
            "l_french:\n my_key:0 \"Bonjour\"\n",
        )
        .unwrap();
        let svc = LocService::from_folder(tmp.path(), ScanBudget::default());
        assert_eq!(svc.files().len(), 2);
        let idx = LocIndex::build(&svc);
        let mut text = LocTextMap::default();
        let mut locs = LocLocationMap::default();
        collect_loc_display(&svc, &idx, Lang::English, false, &mut text, &mut locs);
        let key: std::sync::Arc<str> = "my_key".into();
        let entries = text.get(&key).expect("my_key hover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, Lang::English);
        assert_eq!(entries[0].1, "Hello");
        let mut text_all = LocTextMap::default();
        let mut locs_all = LocLocationMap::default();
        collect_loc_display(
            &svc,
            &idx,
            Lang::English,
            true,
            &mut text_all,
            &mut locs_all,
        );
        let entries_all = text_all.get(&key).unwrap();
        assert_eq!(entries_all.len(), 2);
        assert!(locs.contains_key(&key));
        assert!(locs_all.contains_key(&key));
    }

    #[test]
    fn collect_loc_display_prefers_primary_definition_over_first_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        let loc_dir = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc_dir).unwrap();
        std::fs::write(
            loc_dir.join("a_l_french.yml"),
            "l_french:\n dup_key:0 \"Bonjour\"\n",
        )
        .unwrap();
        std::fs::write(
            loc_dir.join("b_l_english.yml"),
            "l_english:\n dup_key:0 \"Hello\"\n",
        )
        .unwrap();
        let svc = LocService::from_folder(tmp.path(), ScanBudget::default());
        let idx = LocIndex::build(&svc);
        let mut text = LocTextMap::default();
        let mut locs = LocLocationMap::default();
        collect_loc_display(&svc, &idx, Lang::English, false, &mut text, &mut locs);
        let key: std::sync::Arc<str> = "dup_key".into();
        let loc = locs.get(&key).unwrap();
        assert!(
            loc.0.to_string().contains("b_l_english"),
            "primary English file should be the goto target, got {}",
            loc.0
        );
    }
}
