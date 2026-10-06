use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tower_lsp::lsp_types::*;

use cwtools_file_manager::file_manager::FileError;
use cwtools_localization::Lang;
use cwtools_rules::rules_types::RuleSet;

use crate::command_progress::CommandProgress;
use crate::paths::{default_cache_dir, discover_vanilla_dir, path_to_uri};
use crate::{Backend, LocLocations, LocText};

use super::loc::collect_loc_display;

#[allow(clippy::type_complexity)]
pub(crate) fn index_vanilla_dir(
    dir: &std::path::Path,
    ruleset: &RuleSet,
    table: &cwtools_string_table::string_table::StringTable,
    parse_cache_dir: Option<&std::path::Path>,
    game: &str,
) -> Result<
    (
        HashMap<String, Vec<(Arc<str>, cwtools_info::TypeInstance)>>,
        cwtools_info::vanilla_cache::VanillaCacheAux,
    ),
    FileError,
> {
    index_vanilla_dir_with_shadow(dir, ruleset, table, parse_cache_dir, game, None)
}

#[allow(clippy::type_complexity)]
pub(super) fn index_vanilla_dir_with_shadow(
    dir: &std::path::Path,
    ruleset: &RuleSet,
    table: &cwtools_string_table::string_table::StringTable,
    parse_cache_dir: Option<&std::path::Path>,
    game: &str,
    replacement_shadow: Option<&cwtools_file_manager::file_manager::LayerShadow>,
) -> Result<
    (
        HashMap<String, Vec<(Arc<str>, cwtools_info::TypeInstance)>>,
        cwtools_info::vanilla_cache::VanillaCacheAux,
    ),
    FileError,
> {
    let var_effects = cwtools_info::variable_defining_effects(ruleset);
    let index = match (parse_cache_dir, replacement_shadow) {
        (Some(cache_dir), Some(shadow)) => {
            cwtools_driver::index_game_dir_with_parse_cache_and_shadow(
                dir,
                ruleset,
                table,
                &var_effects,
                cache_dir,
                game,
                shadow,
            )
        }
        (None, Some(shadow)) => {
            cwtools_driver::index_game_dir_with_shadow(dir, ruleset, table, &var_effects, shadow)
        }
        (Some(cache_dir), None) => cwtools_driver::index_game_dir_with_parse_cache(
            dir,
            ruleset,
            table,
            &var_effects,
            cache_dir,
            game,
        ),
        (None, None) => cwtools_driver::index_game_dir(dir, ruleset, table, &var_effects),
    }?;
    let aux = match replacement_shadow {
        Some(shadow) => cwtools_driver::build_vanilla_cache_aux_with_shadow(dir, &index, shadow),
        None => cwtools_driver::build_vanilla_cache_aux(dir, &index),
    };
    let per_type = index.map.into_iter().collect();
    Ok((per_type, aux))
}

pub(crate) struct VanillaLoc {
    pub(crate) index: cwtools_localization::LocIndex,
    pub(crate) text: LocText,
    pub(crate) locations: LocLocations,
}

pub(crate) type VanillaLocKey = (std::path::PathBuf, String, Option<Vec<Lang>>, Lang, bool);

impl VanillaLoc {
    pub(crate) fn build(
        service: &cwtools_localization::LocService,
        primary_lang: Lang,
        hover_all: bool,
    ) -> Self {
        let index = cwtools_localization::LocIndex::build_scoped(service, None);
        let mut text = LocText::default();
        let mut locations = LocLocations::default();
        collect_loc_display(
            service,
            &index,
            primary_lang,
            hover_all,
            false,
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

    /// Stage the index and its auxiliary payload together so concurrent cache
    /// loads cannot interleave their two halves (#283).
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
        {
            let mut vanilla = self.state.vanilla_state.lock();
            vanilla.index = Some(per_type);
            if !loc_keys.is_empty() {
                vanilla.loc_keys = Some(loc_keys);
            }
            vanilla.file_paths = Some(file_paths);
            vanilla.var_names = Some(var_names);
            vanilla.scripted_loc_names = Some(scripted_loc_names);
            vanilla.scripted_gui_names = Some(scripted_gui_names);
        }
        self.merge_vanilla_dynamic_values(complex_enum_values, value_set_values);
        total
    }

    pub(crate) fn merge_pending_vanilla_index(&self) {
        let (per_type, var_names, scripted_loc_names, scripted_gui_names, file_paths) = {
            let mut vanilla = self.state.vanilla_state.lock();
            (
                vanilla.index.take(),
                vanilla.var_names.clone(),
                vanilla.scripted_loc_names.clone(),
                vanilla.scripted_gui_names.clone(),
                vanilla.file_paths.clone(),
            )
        };
        let (vanilla_dir, replacement_shadow) = {
            let config = self.state.config.read();
            (
                config.vanilla_dir.clone(),
                config.workspace_roots.first().map(|primary| {
                    cwtools_driver::vanilla_replacement_shadow(primary, &config.parent_roots)
                }),
            )
        };
        if let Some(per_type) = per_type {
            // fell back to whatever document the user had open (#62).
            let per_type = match (&vanilla_dir, &replacement_shadow) {
                (Some(vanilla_root), Some(shadow)) => {
                    cwtools_driver::filter_vanilla_index(per_type, vanilla_root, shadow)
                }
                _ => per_type,
            };
            let vanilla_root = vanilla_dir
                .as_deref()
                .and_then(|root| std::fs::canonicalize(root).ok());
            let mut uri_cache: HashMap<Arc<str>, Option<Arc<str>>> = HashMap::new();
            let mut converted: HashMap<String, Vec<(Arc<str>, cwtools_info::TypeInstance)>> =
                HashMap::with_capacity(per_type.len());
            for (type_name, instances) in per_type {
                let mut out = Vec::with_capacity(instances.len());
                for (path, inst) in instances {
                    let uri = uri_cache
                        .entry(Arc::clone(&path))
                        .or_insert_with(|| {
                            let root = vanilla_root.as_ref()?;
                            let source = std::path::Path::new(path.as_ref());
                            let Ok(canonical) = std::fs::canonicalize(source) else {
                                return None;
                            };
                            if !canonical.starts_with(root) {
                                return None;
                            }
                            Some(Arc::from(path_to_uri(&canonical).as_str()))
                        })
                        .clone();
                    let Some(uri) = uri else {
                        continue;
                    };
                    out.push((uri, inst));
                }
                converted.insert(type_name, out);
            }
            let uris: HashSet<Arc<str>> = uri_cache.into_values().flatten().collect();
            let old = {
                let mut vanilla = self.state.vanilla_state.lock();
                std::mem::replace(&mut vanilla.merged_uris, uris)
            };

            let mut info_guard = self.state.info_service.write();
            let type_index = Arc::make_mut(&mut info_guard.type_index);
            type_index.remove_files(&old);
            type_index.merge_base_game_with_uris(converted);
            type_index.complete = true;
            self.state.vanilla_merged.store(true, Ordering::SeqCst);
            drop(info_guard);
            self.bump_info_revision();
            if !self.state.scan_in_progress.load(Ordering::SeqCst) {
                self.rebuild_alias_key_index();
            }
        }

        // (#306). Installed here rather than during the per-type merge so it
        if let Some(var_names) = var_names {
            let mut info = self.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .var_index
                .set_vanilla_names(var_names);
            drop(info);
            self.bump_info_revision();
        }

        // naming one must resolve without the mod having to define it (#348).
        if let Some(names) = scripted_loc_names {
            let mut info = self.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .scripted_loc_index
                .set_vanilla_names(names);
            drop(info);
            self.bump_info_revision();
        }

        if let Some(names) = scripted_gui_names {
            let mut info = self.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .scripted_gui_index
                .set_vanilla_names(names);
            drop(info);
            self.bump_info_revision();
        }

        let (workspace_root, parent_roots, ignore_files, ignore_dirs) = {
            let config = self.state.config.read();
            let Some(workspace_root) = config.workspace_roots.first().cloned() else {
                return;
            };
            (
                workspace_root,
                config.parent_roots.clone(),
                config.ignore_file_patterns.clone(),
                config.ignore_dir_patterns.clone(),
            )
        };
        let vanilla_paths = match file_paths {
            Some(p) => p,
            None => return,
        };
        let vanilla_paths = if let Some(shadow) = replacement_shadow.as_ref() {
            vanilla_paths
                .into_iter()
                .filter(|path| !shadow.hides(path))
                .collect()
        } else {
            vanilla_paths
        };
        let file_index = cwtools_driver::build_file_index(
            &workspace_root,
            &parent_roots,
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
            && (self.state.vanilla_state.lock().index.is_some()
                || self.state.vanilla_merged.load(Ordering::SeqCst))
        {
            return;
        }
        let (explicit_dir, game, replacement_shadow) = {
            let cfg = self.state.config.read();
            (
                cfg.vanilla_dir.clone(),
                cfg.language.clone(),
                cfg.workspace_roots.first().map(|primary| {
                    cwtools_driver::vanilla_replacement_shadow(primary, &cfg.parent_roots)
                }),
            )
        };
        let has_vanilla_replacements = replacement_shadow
            .as_ref()
            .is_some_and(cwtools_file_manager::file_manager::LayerShadow::has_replace_paths);
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

        let fingerprint = replacement_shadow.as_ref().map_or_else(
            || cwtools_info::vanilla_cache::combined_fingerprint(&dir, &ruleset),
            |shadow| cwtools_driver::replacement_aware_vanilla_fingerprint(&dir, &ruleset, shadow),
        );
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
        let shadow = replacement_shadow.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            if has_vanilla_replacements {
                index_vanilla_dir_with_shadow(
                    &index_dir,
                    &ruleset,
                    &table,
                    parse_cache_dir.as_deref(),
                    &cache_game,
                    shadow.as_ref(),
                )
            } else {
                index_vanilla_dir(
                    &index_dir,
                    &ruleset,
                    &table,
                    parse_cache_dir.as_deref(),
                    &cache_game,
                )
            }
        })
        .await;
        let (per_type, aux) = match join_result {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!(
                            "Vanilla indexing failed for {} — base-game references will not resolve. Error: {}",
                            dir.display(),
                            e
                        ),
                    )
                    .await;
                return;
            }
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
    use cwtools_info::{SourceLocation, TypeInstance};

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

    fn reset_pending_vanilla(backend: &Backend) {
        let mut vanilla = backend.state.vanilla_state.lock();
        vanilla.index = None;
        vanilla.file_paths = None;
        vanilla.loc_keys = None;
        drop(vanilla);
        backend.state.vanilla_merged.store(false, Ordering::SeqCst);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replacement_view_vanilla_cache_is_written_reused_and_scoped() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let vanilla_root = tmp.path().join("vanilla");
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&primary).unwrap();
        for relative in [
            "common/things/replaced.dds",
            "common/things/nested/kept.dds",
            "common/outside/unrelated.dds",
        ] {
            let path = vanilla_root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        std::fs::write(
            primary.join("descriptor.mod"),
            r#"replace_path = "common/things""#,
        )
        .unwrap();
        {
            let mut config = backend.state.config.write();
            config.language = "hoi4".into();
            config.workspace_roots = vec![primary.clone()];
            config.vanilla_dir = Some(vanilla_root.clone());
            config.cache_dir = Some(cache_dir.clone());
            config.refresh_roots();
        }
        backend.set_ruleset(RuleSet::new());

        backend.ensure_vanilla_index(None, false, true).await;

        let shadow = cwtools_driver::vanilla_replacement_shadow(&primary, &[]);
        let fingerprint = cwtools_driver::replacement_aware_vanilla_fingerprint(
            &vanilla_root,
            &RuleSet::new(),
            &shadow,
        );
        let cache_path = backend
            .vanilla_cache_path("hoi4", &fingerprint)
            .expect("configured cache directory");
        assert!(cache_path.exists(), "replacement view cache is written");
        let (_, _, cached) = cwtools_info::vanilla_cache::load(&cache_path).unwrap();
        assert!(
            !cached
                .aux
                .file_paths
                .iter()
                .any(|path| path.ends_with("common/things/replaced.dds"))
        );
        let nested_path = "common/things/nested/kept.dds";
        assert!(
            cached
                .aux
                .file_paths
                .iter()
                .any(|path| path.ends_with(nested_path))
        );

        std::fs::remove_file(vanilla_root.join(nested_path)).unwrap();
        reset_pending_vanilla(&backend);
        backend.ensure_vanilla_index(None, false, true).await;
        assert!(
            backend
                .state
                .vanilla_state
                .lock()
                .file_paths
                .as_ref()
                .is_some_and(|paths| paths.iter().any(|path| path.ends_with(nested_path))),
            "a same-view cache hit retains its indexed source paths"
        );

        std::fs::write(
            primary.join("descriptor.mod"),
            r#"replace_path = "common/outside""#,
        )
        .unwrap();
        reset_pending_vanilla(&backend);
        backend.ensure_vanilla_index(None, false, true).await;
        assert_eq!(
            std::fs::read_dir(&cache_dir)
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "cwv"))
                .count(),
            2,
            "a different replacement view gets its own aggregate cache"
        );
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
    fn merge_drops_instances_outside_configured_vanilla_root() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let vanilla_root = tmp.path().join("vanilla");
        let outside_root = tmp.path().join("outside");
        std::fs::create_dir_all(vanilla_root.join("nested")).unwrap();
        std::fs::create_dir_all(&outside_root).unwrap();
        let in_root = vanilla_root.join("nested").join("..").join("in.txt");
        let out_of_root = outside_root.join("out.txt");
        std::fs::write(&in_root, "").unwrap();
        std::fs::write(&out_of_root, "").unwrap();
        let canonical_in_root = std::fs::canonicalize(&in_root).unwrap();

        {
            let mut config = backend.state.config.write();
            config.vanilla_dir = Some(vanilla_root.clone());
            config.refresh_roots();
        }

        let instance = |name: &str| TypeInstance {
            name: name.to_string(),
            location: SourceLocation {
                line: 0,
                col: 0,
                end: (0, 1),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        };
        let mut data = vanilla_data(Vec::new());
        data.per_type.insert(
            "foo".to_string(),
            vec![
                (
                    Arc::from(in_root.to_string_lossy().as_ref()),
                    instance("in_root"),
                ),
                (
                    Arc::from(out_of_root.to_string_lossy().as_ref()),
                    instance("out_of_root"),
                ),
            ],
        );
        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index();

        let info = backend.state.info_service.read();
        let entries = info.type_index.map.get("foo").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.name, "in_root");
        assert_eq!(entries[0].0.as_ref(), path_to_uri(&canonical_in_root));
    }

    #[test]
    fn merge_suppresses_vanilla_replaced_by_primary_and_parent_mods() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let parent = tmp.path().join("parent");
        let vanilla_root = tmp.path().join("vanilla");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(
            primary.join("descriptor.mod"),
            "name = \"Primary\"\nreplace_path = \"common/scripted_effects/primary\"\n",
        )
        .unwrap();
        std::fs::write(
            parent.join("descriptor.mod"),
            "name = \"Parent\"\nreplace_path = \"COMMON/scripted_effects/PARENT\"\n",
        )
        .unwrap();

        let entries = [
            (
                "common/scripted_effects/Primary/hidden.txt",
                "primary_hidden",
            ),
            (
                "common/scripted_effects/Primary/nested/kept.txt",
                "primary_nested",
            ),
            ("common/scripted_effects/parent/hidden.txt", "parent_hidden"),
            (
                "common/scripted_effects/parent_extra/kept.txt",
                "parent_sibling",
            ),
            ("common/scripted_effects/elsewhere/kept.txt", "unreplaced"),
        ];
        let instance = |name: &str| TypeInstance {
            name: name.to_string(),
            location: SourceLocation {
                line: 0,
                col: 0,
                end: (0, 1),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        };
        let mut data = vanilla_data(Vec::new());
        data.aux.file_paths = vec![
            "common/scripted_effects/primary/replaced.dds".to_string(),
            "common/scripted_effects/primary/assets/kept.dds".to_string(),
            "common/scripted_effects/parent_extra/assets/kept.dds".to_string(),
        ];
        let mut vanilla_instances = Vec::new();
        for (relative, name) in entries {
            let path = vanilla_root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            vanilla_instances.push((Arc::from(path.to_string_lossy().as_ref()), instance(name)));
        }
        data.per_type
            .insert("scripted_effect".to_string(), vanilla_instances);
        {
            let mut config = backend.state.config.write();
            config.workspace_roots = vec![primary.clone()];
            config.parent_roots = vec![parent.clone()];
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;

                let alias = tmp.path().join("vanilla-alias");
                symlink(&vanilla_root, &alias).unwrap();
                config.vanilla_dir = Some(alias);
            }
            #[cfg(not(unix))]
            {
                config.vanilla_dir = Some(vanilla_root.clone());
            }
            config.refresh_roots();
        }

        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index();

        let info = backend.state.info_service.read();
        let mut names: Vec<&str> = info
            .type_index
            .instances("scripted_effect")
            .iter()
            .map(|(_, instance)| instance.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["parent_sibling", "primary_nested", "unreplaced"]
        );
        let file_index = &info.type_index.file_index;
        assert!(!file_index.contains("common/scripted_effects/primary/replaced.dds"));
        assert!(file_index.contains("common/scripted_effects/primary/assets/kept.dds"));
        assert!(file_index.contains("common/scripted_effects/parent_extra/assets/kept.dds"));
    }

    #[test]
    fn index_vanilla_dir_with_replace_path_filters_auxiliary_paths_and_localisation() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let vanilla_root = tmp.path().join("vanilla");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::write(
            primary.join("descriptor.mod"),
            r#"replace_path = "localisation/replaced"
replace_path = "gfx/replaced"
"#,
        )
        .unwrap();
        let hidden_loc = vanilla_root.join("localisation/replaced/hidden.yml");
        let kept_loc = vanilla_root.join("localisation/kept.yml");
        let hidden_asset = vanilla_root.join("gfx/replaced/hidden.dds");
        let kept_asset = vanilla_root.join("gfx/kept/visible.dds");
        for path in [&hidden_loc, &kept_loc, &hidden_asset, &kept_asset] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        std::fs::write(&hidden_loc, "l_english:\n hidden_key:0 \"Hidden\"\n").unwrap();
        std::fs::write(&kept_loc, "l_english:\n kept_key:0 \"Kept\"\n").unwrap();
        std::fs::write(hidden_asset, b"").unwrap();
        std::fs::write(kept_asset, b"").unwrap();

        let shadow = cwtools_driver::vanilla_replacement_shadow(&primary, &[]);
        let table = cwtools_string_table::string_table::StringTable::new();
        let (_per_type, aux) = index_vanilla_dir_with_shadow(
            &vanilla_root,
            &RuleSet::new(),
            &table,
            None,
            "hoi4",
            Some(&shadow),
        )
        .unwrap();

        assert!(
            aux.file_paths
                .iter()
                .any(|path| path.ends_with("gfx/kept/visible.dds"))
        );
        assert!(
            !aux.file_paths
                .iter()
                .any(|path| path.ends_with("gfx/replaced/hidden.dds"))
        );
        let loc_keys: Vec<&str> = aux
            .loc_keys
            .iter()
            .flat_map(|(_, keys)| keys.iter().map(String::as_str))
            .collect();
        assert!(
            loc_keys
                .iter()
                .any(|key| key.eq_ignore_ascii_case("kept_key"))
        );
        assert!(
            !loc_keys
                .iter()
                .any(|key| key.eq_ignore_ascii_case("hidden_key"))
        );
    }

    #[test]
    fn re_merge_replaces_auxiliary_names() {
        let backend = test_backend();
        let mut old = vanilla_data(vec!["old_var"]);
        old.aux.scripted_loc_names = vec!["old_scripted_loc".into()];
        old.aux.scripted_gui_names = vec!["old_scripted_gui".into()];
        backend.stage_vanilla_payload(old);
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

        let mut new = vanilla_data(vec!["new_var"]);
        new.aux.scripted_loc_names = vec!["new_scripted_loc".into()];
        new.aux.scripted_gui_names = vec!["new_scripted_gui".into()];
        backend.stage_vanilla_payload(new);
        backend.merge_pending_vanilla_index();

        let idx = backend.state.info_service.read();
        assert!(
            !idx.type_index.var_index.contains("old_var"),
            "re-merge must replace"
        );
        assert!(idx.type_index.var_index.contains("new_var"));
        assert!(
            !idx.type_index
                .scripted_loc_index
                .contains("old_scripted_loc")
        );
        assert!(
            idx.type_index
                .scripted_loc_index
                .contains("new_scripted_loc")
        );
        assert!(
            !idx.type_index
                .scripted_gui_index
                .contains("old_scripted_gui")
        );
        assert!(
            idx.type_index
                .scripted_gui_index
                .contains("new_scripted_gui")
        );
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
        backend.state.vanilla_state.lock().var_names = None;
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
