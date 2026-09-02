use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tower_lsp::lsp_types::*;

use cwtools_localization::Lang;
use cwtools_rules::rules_types::RuleSet;

use crate::command_progress::CommandProgress;
use crate::paths::{default_cache_dir, discover_vanilla_dir, path_to_uri};
use crate::{Backend, LocLocationMap, LocTextMap};

use super::loc::collect_loc_display;

#[allow(clippy::type_complexity)]
pub(crate) fn index_vanilla_dir(
    dir: &std::path::Path,
    ruleset: &RuleSet,
    table: &cwtools_string_table::string_table::StringTable,
    parse_cache_dir: Option<&std::path::Path>,
    game: &str,
) -> (
    HashMap<String, Vec<(Arc<str>, cwtools_info::TypeInstance)>>,
    cwtools_info::vanilla_cache::VanillaCacheAux,
) {
    let var_effects = cwtools_info::variable_defining_effects(ruleset);
    let index = match parse_cache_dir {
        Some(cache_dir) => cwtools_driver::index_game_dir_with_parse_cache(
            dir,
            ruleset,
            table,
            &var_effects,
            cache_dir,
            game,
        ),
        None => cwtools_driver::index_game_dir(dir, ruleset, table, &var_effects),
    };
    let aux = cwtools_driver::build_vanilla_cache_aux(dir, &index);
    let per_type = index.map.into_iter().collect();
    (per_type, aux)
}

pub(crate) struct VanillaLoc {
    pub(crate) index: cwtools_localization::LocIndex,
    pub(crate) text: LocTextMap,
    pub(crate) locations: LocLocationMap,
}

pub(crate) type VanillaLocKey = (std::path::PathBuf, Option<Vec<Lang>>, Lang, bool);

impl VanillaLoc {
    pub(crate) fn build(
        service: &cwtools_localization::LocService,
        primary_lang: Lang,
        hover_all: bool,
    ) -> Self {
        let index = cwtools_localization::LocIndex::build_scoped(service, None);
        let mut text = LocTextMap::default();
        let mut locations = LocLocationMap::default();
        collect_loc_display(
            service,
            &index,
            primary_lang,
            hover_all,
            &mut text,
            &mut locations,
        );
        Self {
            index,
            text,
            locations,
        }
    }
}

impl Backend {
    pub(crate) fn merge_vanilla_dynamic_values(
        &self,
        complex_enums: Vec<(String, Vec<String>)>,
        value_sets: Vec<(String, Vec<String>)>,
    ) {
        if complex_enums.is_empty() && value_sets.is_empty() {
            return;
        }
        let mut info = self.state.info_service.write();
        let type_index = Arc::make_mut(&mut info.type_index);
        type_index
            .complex_enum_values
            .merge_file("<vanilla-dynamic>", complex_enums.into_iter().collect());
        type_index
            .value_set_values
            .merge_file("<vanilla-dynamic>", value_sets.into_iter().collect());
        drop(info);
        self.bump_info_revision();
    }

    /// error rather than one the editor silently drops (#283).
    pub(crate) fn stage_vanilla_payload(
        &self,
        data: cwtools_info::vanilla_cache::VanillaCacheData,
    ) -> usize {
        let cwtools_info::vanilla_cache::VanillaCacheData { per_type, aux } = data;
        let cwtools_info::vanilla_cache::VanillaCacheAux {
            loc_keys,
            file_paths,
            var_names,
            complex_enum_values,
            value_set_values,
            scripted_loc_names,
            scripted_gui_names,
        } = aux;

        let total: usize = per_type.values().map(|v| v.len()).sum();
        *self.state.vanilla_index.lock() = Some(per_type);
        if !loc_keys.is_empty() {
            *self.state.vanilla_loc_keys.lock() = Some(loc_keys);
        }
        *self.state.vanilla_file_paths.lock() = Some(file_paths);
        *self.state.vanilla_var_names.lock() = Some(var_names);
        *self.state.vanilla_scripted_loc_names.lock() = Some(scripted_loc_names);
        *self.state.vanilla_scripted_gui_names.lock() = Some(scripted_gui_names);
        self.merge_vanilla_dynamic_values(complex_enum_values, value_set_values);
        total
    }

    pub(crate) fn merge_pending_vanilla_index(&self) {
        let per_type = self.state.vanilla_index.lock().take();
        if let Some(per_type) = per_type {
            // fell back to whatever document the user had open (#62).
            let mut uri_cache: HashMap<Arc<str>, Arc<str>> = HashMap::new();
            let mut converted: HashMap<String, Vec<(Arc<str>, cwtools_info::TypeInstance)>> =
                HashMap::with_capacity(per_type.len());
            for (type_name, instances) in per_type {
                let mut out = Vec::with_capacity(instances.len());
                for (path, inst) in instances {
                    let uri = uri_cache
                        .entry(path)
                        .or_insert_with_key(|p| {
                            Arc::from(path_to_uri(std::path::Path::new(p.as_ref())).as_str())
                        })
                        .clone();
                    out.push((uri, inst));
                }
                converted.insert(type_name, out);
            }
            let uris: HashSet<Arc<str>> = uri_cache.into_values().collect();
            let old = {
                let mut merged = self.state.vanilla_merged_uris.lock();
                std::mem::replace(&mut *merged, uris)
            };

            let mut info_guard = self.state.info_service.write();
            let type_index = Arc::make_mut(&mut info_guard.type_index);
            type_index.remove_files(&old);
            type_index.merge_base_game_with_uris(converted);
            type_index.complete = true;
            self.state.vanilla_merged.store(true, Ordering::SeqCst);
            drop(info_guard);
            self.bump_info_revision();
        }

        // (#306). Installed here rather than during the per-type merge so it
        if let Some(var_names) = self.state.vanilla_var_names.lock().clone() {
            let mut info = self.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .var_index
                .set_vanilla_names(var_names);
            drop(info);
            self.bump_info_revision();
        }

        // naming one must resolve without the mod having to define it (#348).
        if let Some(names) = self.state.vanilla_scripted_loc_names.lock().clone() {
            let mut info = self.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .scripted_loc_index
                .set_vanilla_names(names);
            drop(info);
            self.bump_info_revision();
        }

        if let Some(names) = self.state.vanilla_scripted_gui_names.lock().clone() {
            let mut info = self.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .scripted_gui_index
                .set_vanilla_names(names);
            drop(info);
            self.bump_info_revision();
        }

        let (workspace_root, ignore_files, ignore_dirs) = {
            let config = self.state.config.read();
            let Some(workspace_root) = config.workspace_roots.first().cloned() else {
                return;
            };
            (
                workspace_root,
                config.ignore_file_patterns.clone(),
                config.ignore_dir_patterns.clone(),
            )
        };
        let vanilla_paths = match self.state.vanilla_file_paths.lock().clone() {
            Some(p) => p,
            None => return,
        };
        let file_index = cwtools_driver::build_file_index(
            &workspace_root,
            &ignore_files,
            &ignore_dirs,
            cwtools_driver::VanillaFiles::Cached(vanilla_paths),
            false,
        );
        {
            let mut info_guard = self.state.info_service.write();
            Arc::make_mut(&mut info_guard.type_index).file_index = file_index;
        }
        self.bump_info_revision();
    }

    pub(crate) async fn ensure_vanilla_index(
        &self,
        progress: Option<&CommandProgress>,
        force_rebuild: bool,
        quiet: bool,
    ) {
        if !force_rebuild
            && (self.state.vanilla_index.lock().is_some()
                || self.state.vanilla_merged.load(Ordering::SeqCst))
        {
            return;
        }
        let (explicit_dir, game) = {
            let cfg = self.state.config.read();
            (cfg.vanilla_dir.clone(), cfg.language.clone())
        };
        let was_explicit = explicit_dir.is_some();
        let dir = explicit_dir.or_else(|| discover_vanilla_dir(&game));
        let dir = match dir {
            Some(d) if d.is_dir() => d,
            _ => return,
        };
        if !was_explicit {
            let mut cfg = self.state.config.write();
            cfg.vanilla_dir = Some(dir.clone());
            cfg.refresh_roots();
        }

        let ruleset_opt = self.state.rules.read().ruleset.clone();
        let ruleset = match ruleset_opt {
            Some(rs) => rs,
            None => {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        "Base-game dir set but no rules loaded yet; skipping vanilla index.",
                    )
                    .await;
                return;
            }
        };

        let fingerprint = cwtools_info::vanilla_cache::combined_fingerprint(&dir, &ruleset);
        let cache_path = self.vanilla_cache_path(&game, &fingerprint);

        if !force_rebuild
            && let Some(cp) = &cache_path
            && cp.exists()
        {
            match cwtools_info::vanilla_cache::load(cp) {
                Ok((cache_game, cache_fp, data))
                    if cache_game == game && cache_fp == fingerprint =>
                {
                    let total = self.stage_vanilla_payload(data);
                    self.client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "Loaded {} base-game instances from cache {} ({})",
                                total,
                                cp.display(),
                                fingerprint
                            ),
                        )
                        .await;
                    return;
                }
                Ok((_, cache_fp, _)) => {
                    self.client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "Vanilla cache stale (cached {}, install {}); rebuilding",
                                cache_fp, fingerprint
                            ),
                        )
                        .await;
                }
                Err(e) => {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("Could not load vanilla cache {}: {}", cp.display(), e),
                        )
                        .await;
                }
            }
        }

        if !quiet {
            self.send_loading_bar_pct(progress, true, "Indexing base game…", None)
                .await;
        }
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "Indexing base game at {} ({}) …",
                    dir.display(),
                    fingerprint
                ),
            )
            .await;

        let parse_cache_dir = if force_rebuild {
            None
        } else {
            cache_path
                .as_deref()
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf)
        };
        let table = self.state.string_table.clone();
        let index_dir = dir.clone();
        let cache_game = game.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            index_vanilla_dir(
                &index_dir,
                &ruleset,
                &table,
                parse_cache_dir.as_deref(),
                &cache_game,
            )
        })
        .await;
        let (per_type, aux) = match join_result {
            Ok(result) => result,
            Err(e) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!(
                            "Vanilla indexing task failed for {} — base-game references will not resolve. Error: {}",
                            dir.display(),
                            e
                        ),
                    )
                    .await;
                tracing::error!("spawn_blocking vanilla index panicked: {}", e);
                return;
            }
        };

        if let Some(cp) = &cache_path {
            match cwtools_info::vanilla_cache::save_per_type(
                &per_type,
                &game,
                &fingerprint,
                cp,
                aux.clone(),
            ) {
                Ok(n) => {
                    self.client
                        .log_message(
                            MessageType::INFO,
                            format!(
                                "Cached {} base-game instances to {} ({})",
                                n,
                                cp.display(),
                                fingerprint
                            ),
                        )
                        .await
                }
                Err(e) => {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("Could not write vanilla cache {}: {}", cp.display(), e),
                        )
                        .await
                }
            }
        }

        let total = self
            .stage_vanilla_payload(cwtools_info::vanilla_cache::VanillaCacheData { per_type, aux });
        self.client
            .log_message(
                MessageType::INFO,
                format!("Indexed {} base-game instances.", total),
            )
            .await;
    }

    pub(crate) fn vanilla_cache_path(
        &self,
        game: &str,
        fingerprint: &str,
    ) -> Option<std::path::PathBuf> {
        let base = self
            .state
            .config
            .read()
            .cache_dir
            .clone()
            .or_else(default_cache_dir)?;
        Some(base.join(cwtools_info::vanilla_cache::cache_file_name(
            game,
            fingerprint,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use cwtools_info::vanilla_cache::{VanillaCacheAux, VanillaCacheData};

    use crate::state::DocumentState;

    fn test_backend() -> Backend {
        let state = Arc::new(DocumentState::new());
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let slot = captured.clone();
        let server_state = state.clone();
        let (_service, _socket) = tower_lsp::LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let client = captured.lock().take().unwrap();
        Backend { client, state }
    }

    fn vanilla_data(var_names: Vec<&str>) -> VanillaCacheData {
        VanillaCacheData {
            per_type: HashMap::new(),
            aux: VanillaCacheAux {
                loc_keys: Vec::new(),
                file_paths: Vec::new(),
                var_names: var_names.into_iter().map(|s| s.to_string()).collect(),
                complex_enum_values: Vec::new(),
                value_set_values: Vec::new(),
                scripted_loc_names: Vec::new(),
                scripted_gui_names: Vec::new(),
            },
        }
    }

    #[test]
    fn stage_does_not_install_before_merge() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["vanilla_var"]));
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("vanilla_var")
        );
    }

    #[test]
    fn merge_installs_after_stage() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["vanilla_var"]));
        backend.merge_pending_vanilla_index();
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("vanilla_var")
        );
    }

    #[test]
    fn re_merge_replaces_not_accumulates() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["old_var"]));
        backend.merge_pending_vanilla_index();
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("old_var")
        );
        backend.stage_vanilla_payload(vanilla_data(vec!["new_var"]));
        backend.merge_pending_vanilla_index();
        let idx = backend.state.info_service.read();
        assert!(
            !idx.type_index.var_index.contains("old_var"),
            "re-merge must replace"
        );
        assert!(idx.type_index.var_index.contains("new_var"));
    }

    #[test]
    fn clear_file_on_shared_name_does_not_strip_vanilla() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["shared_var"]));
        backend.merge_pending_vanilla_index();
        {
            let mut info = backend.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .var_index
                .add_name("shared_var");
        }
        {
            let mut info = backend.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .var_index
                .remove_name("shared_var");
        }
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("shared_var"),
            "vanilla must survive mod clear_file on same name"
        );
    }

    #[test]
    fn clear_all_caches_drops_vanilla_vars() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["vanilla_var"]));
        backend.merge_pending_vanilla_index();
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("vanilla_var")
        );
        *backend.state.vanilla_var_names.lock() = None;
        {
            let mut info = backend.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .var_index
                .clear_vanilla_names();
        }
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("vanilla_var")
        );
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .is_empty()
        );
    }

    #[test]
    fn staged_empty_vanilla_clears_previous() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["a"]));
        backend.merge_pending_vanilla_index();
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("a")
        );
        backend.stage_vanilla_payload(vanilla_data(vec![]));
        backend.merge_pending_vanilla_index();
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("a")
        );
    }

    #[test]
    fn vanilla_scripted_gui_callbacks_are_merged_case_insensitively() {
        let backend = test_backend();
        let mut data = vanilla_data(Vec::new());
        data.aux.scripted_gui_names = vec!["Topbar_Icon_Click".into()];
        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index();

        let info = backend.state.info_service.read();
        assert!(
            info.type_index
                .scripted_gui_index
                .contains("TOPBAR_ICON_CLICK")
        );
    }

    #[test]
    fn vanilla_vars_reach_loc_bindable_names() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["vanilla_loc_var"]));
        backend.merge_pending_vanilla_index();
        let idx = backend.state.info_service.read();
        let names: std::collections::HashSet<String> =
            idx.type_index.loc_bindable_names().collect();
        assert!(names.contains("vanilla_loc_var"));
    }

    #[test]
    fn vanilla_makes_index_non_empty() {
        let backend = test_backend();
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .is_empty()
        );
        backend.stage_vanilla_payload(vanilla_data(vec!["vanilla_only"]));
        backend.merge_pending_vanilla_index();
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .is_empty()
        );
    }

    #[test]
    fn merge_sets_vanilla_merged_flag() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["vanilla_only"]));
        backend.merge_pending_vanilla_index();
        assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
    }

    #[test]
    fn vanilla_vars_case_insensitive_via_vanilla_provenance() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["VANILLA_VAR"]));
        backend.merge_pending_vanilla_index();
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .var_index
                .contains("vanilla_var")
        );
        let names: std::collections::HashSet<String> = backend
            .state
            .info_service
            .read()
            .type_index
            .loc_bindable_names()
            .collect();
        assert!(names.contains("vanilla_var"));
    }
}
