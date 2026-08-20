use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tower_lsp::lsp_types::*;

use cwtools_localization::Lang;

use crate::paths::{loc_display_text, path_to_uri};
use crate::validate::{DocLines, loc_diag_to_validation_error, validation_error_to_diagnostic};
use crate::{Backend, LocLocationMap, LocTextMap};

use super::{VanillaLoc, stat_signature_for};

/// Extract the per-key hover text and a representative definition site from a
/// loaded [`LocService`], keyed by `index`'s interned keys so the maps share
/// their allocations with the loc index.
///
/// `text` accumulates every included language's display string per key, in file
/// order. `locations` keeps one site per key, preferring `primary_lang` so
/// Ctrl+Click lands on the canonical entry rather than whichever language was
/// scanned first. Shared by the workspace walk and the base-game memo so the
/// two can't drift.
pub(crate) fn collect_loc_display(
    service: &cwtools_localization::LocService,
    index: &cwtools_localization::LocIndex,
    primary_lang: Lang,
    hover_all: bool,
    text: &mut LocTextMap,
    locations: &mut LocLocationMap,
) {
    for file in service.files() {
        // A file whose header language the parser didn't recognise is absent
        // from the key index; hover and goto still show it, under English.
        let lang = file.lang.unwrap_or(Lang::English);
        let lang_included = hover_all || lang == primary_lang;
        // Every entry in a file shares the same source path.
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
    /// Stat-only signature (path, size, mtime) over the loc files
    /// `rebuild_and_publish_loc` re-reads on every scan. Lets a quiet background
    /// pass detect "nothing loc-related changed" and skip the full rebuild
    /// without reading or parsing a single file. Discovers files via
    /// `LocService::discover_files_filtered` — the exact filtered walk
    /// `rebuild_and_publish_loc` uses — so this can't drift from what it reads.
    /// The base-game install is deliberately absent: it can't change while the
    /// editor is running and its contribution is memoized by
    /// [`Backend::vanilla_loc`], so stat'ing its ~2000 loc files every pass
    /// would only cost time. Blocking (stats every discovered file); call from
    /// within `block_in_place`.
    pub(crate) fn compute_loc_signature(&self, root_path: &std::path::Path) -> u64 {
        let (ignore_files, ignore_dirs) = {
            let config = self.state.config.read();
            (
                config.ignore_file_patterns.clone(),
                config.ignore_dir_patterns.clone(),
            )
        };
        let files = cwtools_localization::LocService::discover_files_filtered(
            &[root_path],
            cwtools_file_manager::file_manager::ScanBudget::default(),
            &ignore_files,
            &ignore_dirs,
        );
        stat_signature_for(&files)
    }

    /// The base-game install's contribution to the loc maps, built on first use
    /// and kept for the rest of the session.
    ///
    /// Vanilla loc is ~2000 files / 150 MB on HOI4 and it cannot change while
    /// the editor is running, yet every foreground scan used to re-read and
    /// re-parse all of it just to rebuild the same hover text, the same
    /// definition sites and the same keys (#89).
    ///
    /// Keyed by the inputs that shape the maps: the install dir, selected
    /// languages, primary language and hover-all-languages toggle.
    ///
    /// `None` when no base-game dir is configured, and also when the configured
    /// one yielded no loc at all (an install on a drive that isn't mounted):
    /// nothing is memoized then, so the next scan tries again, and the caller
    /// keeps falling back to the vanilla cache's keys meanwhile. Blocking; call
    /// from within `block_in_place`.
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
        let service = cwtools_localization::LocService::from_folders_filtered(
            &[&key.0],
            cwtools_file_manager::file_manager::ScanBudget::default(),
            key.1.as_deref(),
            &[],
            &[],
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

    /// Build the loc-key index from the workspace root plus the vanilla install,
    /// store it in state (for CW100/CW122 on config files), and publish loc-file
    /// diagnostics (CW225/CW234/CW259/CW268/CW275) for the workspace loc files.
    #[tracing::instrument(skip_all)]
    pub(crate) async fn rebuild_and_publish_loc(&self, root_path: &std::path::Path) {
        // The base game's loc files are read whenever the install dir is known,
        // so hover shows translations for keys that exist only there (#51). The
        // vanilla cache's key lists stand in when it isn't. Either way it costs
        // one read per session, not one per scan (#89).
        let (loc_languages, ignore_files, ignore_dirs) = {
            let config = self.state.config.read();
            (
                config.loc_languages.clone(),
                config.ignore_file_patterns.clone(),
                config.ignore_dir_patterns.clone(),
            )
        };

        // Hover language scope: unless the user opted into all translations, keep
        // only the primary language (first configured loc language, else English)
        // in the hover map so it stays small.
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

        // Build the index and collect per-file diagnostics in one block, then
        // drop the LocService before the index is published. The service holds
        // the full per-file loc ASTs (~2M entries on Millennium Dawn); keeping
        // it alive while we also hold the lowercased key set in LocIndex
        // pushes peak RSS by hundreds of MiB for no reason. After the block
        // closes only LocIndex (keys) and the diagnostic map survive.
        // Names a `$ref$` may resolve to besides loc keys: `$modifier$` / `$idea$`
        // embeds resolve against those registries (mirrors the CLI/driver path).
        // Cached vanilla keys are resolved via the loc-index union passed to
        // `validate_loc_project_with_union`, so they aren't duplicated here.
        let extra_valid_refs: HashSet<String> = {
            // Lock order: rules -> info_service.
            let modifier_keys = self.state.rules.read().modifier_keys.clone();
            let info = self.state.info_service.read();
            crate::validate::loc_extra_valid_refs(&modifier_keys, &info.type_index)
        };

        // block_in_place: the loc service reads and parses hundreds of loc files
        // from disk — synchronous I/O that must not starve the async executor.
        let (loc_index, mut by_file, loc_text_map, loc_loc_map, source_hashes) =
            tokio::task::block_in_place(|| {
                // The base game is read once per session and reused; only the
                // workspace is walked again (#89).
                let vanilla = self.vanilla_loc(parsed_languages, primary_lang, hover_all);
                // The install dir is the source of truth for its own loc: the
                // memo walked exactly the tree the vanilla cache's key lists
                // were extracted from, so keeping those around is a second copy
                // of 1.3M keys (as owned strings) that every rebuild would
                // re-intern. Hand them over. Without a dir they're all we have.
                let cached_vanilla_loc = if vanilla.is_some() {
                    self.state.vanilla_loc_keys.lock().take()
                } else {
                    self.state.vanilla_loc_keys.lock().clone()
                };
                let service = cwtools_localization::LocService::from_folders_filtered(
                    &[root_path],
                    cwtools_file_manager::file_manager::ScanBudget::default(),
                    parsed_languages,
                    &ignore_files,
                    &ignore_dirs,
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
                // Reuse the merged loc-index union (with the base game's keys)
                // instead of rebuilding the ~2M-key set inside the validate pass.
                // Every file the service holds is under the workspace root, so
                // there is nothing to filter out — the base game is never
                // validated, only indexed.
                for d in cwtools_localization::validate_loc_project_with_union(
                    &service,
                    loc_languages.as_deref(),
                    idx.union(),
                    &extra_valid_refs,
                ) {
                    let ve = loc_diag_to_validation_error(&d);
                    // Project-wide loc scan feeds the Problems panel; open files
                    // get whole-line squiggles and encoded columns when
                    // re-validated on open.
                    by_file
                        .entry(d.file.clone())
                        .or_default()
                        .push(validation_error_to_diagnostic(&ve, &DocLines::none()));
                }
                // Extract per-key display text for hover and a representative
                // definition site (for goto) before dropping the service.
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
                // Fold the base game in under the workspace: a key the mod
                // redefines keeps the mod's definition site, and its hover shows
                // the mod's text first.
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
        // Prefix-searchable companion for loc completion, built here so the
        // per-keystroke path never pays for it. block_in_place: sorting ~400K
        // keys is CPU-bound and must not sit on the async executor.
        let loc_key_index = tokio::task::block_in_place(|| {
            Arc::new(crate::completion::LocKeyIndex::build(
                loc_index.union().iter().map(AsRef::as_ref),
            ))
        });
        *self.state.loc_index.write() = Some(Arc::new(loc_index));
        *self.state.loc_key_index.write() = Some(loc_key_index);
        *self.state.loc_text.write() = loc_text_map;
        *self.state.loc_locations.write() = loc_loc_map;

        // Publish per-file loc diagnostics, but only for workspace loc files
        // (not vanilla). Open loc documents are revalidated from their live
        // buffers after the index is installed, so disk diagnostics must not
        // overwrite them here.
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
                // Inline `# cwtools-ignore` directives: the index build drops
                // each loc file's text, so read it back just for the files that
                // reported something (the rare ones), not the whole tree. On a
                // blocking thread, like every other capped disk read here.
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
        // Two files: English and French, same key different text.
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
        // Hover only primary (English) -> one translation.
        let mut text = LocTextMap::default();
        let mut locs = LocLocationMap::default();
        collect_loc_display(&svc, &idx, Lang::English, false, &mut text, &mut locs);
        let key: std::sync::Arc<str> = "my_key".into();
        let entries = text.get(&key).expect("my_key hover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, Lang::English);
        assert_eq!(entries[0].1, "Hello");
        // Hover all -> both languages.
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
        // Locations: primary definition is preferred.
        assert!(locs.contains_key(&key));
        assert!(locs_all.contains_key(&key));
    }

    #[test]
    fn collect_loc_display_prefers_primary_definition_over_first_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        let loc_dir = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc_dir).unwrap();
        // Write French first (lexicographically earlier) but primary is English.
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
