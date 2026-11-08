use std::borrow::Cow;
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

type VanillaIndexPayload = (
    HashMap<String, Vec<(Arc<str>, cwtools_info::TypeInstance)>>,
    cwtools_info::vanilla_cache::VanillaCacheAux,
);
type VanillaIndexResult = Result<VanillaIndexPayload, FileError>;
pub(crate) type VanillaLoader = fn(
    &std::path::Path,
    &RuleSet,
    &cwtools_string_table::string_table::StringTable,
    Option<&std::path::Path>,
    &str,
    Option<&cwtools_file_manager::file_manager::LayerShadow>,
) -> VanillaIndexResult;

pub(crate) fn index_vanilla_dir(
    dir: &std::path::Path,
    ruleset: &RuleSet,
    table: &cwtools_string_table::string_table::StringTable,
    parse_cache_dir: Option<&std::path::Path>,
    game: &str,
) -> VanillaIndexResult {
    index_vanilla_dir_with_shadow(dir, ruleset, table, parse_cache_dir, game, None)
}

pub(super) fn index_vanilla_dir_with_shadow(
    dir: &std::path::Path,
    ruleset: &RuleSet,
    table: &cwtools_string_table::string_table::StringTable,
    parse_cache_dir: Option<&std::path::Path>,
    game: &str,
    replacement_shadow: Option<&cwtools_file_manager::file_manager::LayerShadow>,
) -> VanillaIndexResult {
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
    }?;
    let per_type = index.into_map().into_iter().collect();
    Ok((per_type, aux))
}

// The driver has its own budgeted auxiliary-discovery tests. This private loader
// injects the same failure at the LSP acquisition boundary to test both callers.
#[cfg(test)]
pub(crate) fn test_capped_vanilla_loader(
    dir: &std::path::Path,
    ruleset: &RuleSet,
    table: &cwtools_string_table::string_table::StringTable,
    parse_cache_dir: Option<&std::path::Path>,
    game: &str,
    shadow: Option<&cwtools_file_manager::file_manager::LayerShadow>,
) -> VanillaIndexResult {
    use cwtools_file_manager::file_manager::{FileKind, FileManagerConfig, ScanBudget};
    let config = FileManagerConfig {
        include_dirs: vec![".".into()],
        scan_budget: ScanBudget {
            max_files: 1,
            ..ScanBudget::default()
        },
        ..Default::default()
    };
    let report = cwtools_driver::discover_workspace(cwtools_driver::WorkspaceDiscoveryRequest {
        roots: vec![dir.to_path_buf()],
        kinds: vec![FileKind::Script, FileKind::Localisation, FileKind::Resource],
        config,
        policy: cwtools_driver::DiscoveryPolicy::Vanilla,
    })?;
    if let Some(failure) = report
        .failures
        .iter()
        .find(|failure| failure.is_file_count_budget())
    {
        return Err(FileError::FileCountBudget(failure.path.clone()));
    }
    index_vanilla_dir_with_shadow(dir, ruleset, table, parse_cache_dir, game, shadow)
}

pub(crate) struct VanillaLoc {
    pub(crate) index: cwtools_localization::LocIndex,
    pub(crate) text: LocText,
    pub(crate) locations: LocLocations,
}

pub(crate) type VanillaLocKey = (std::path::PathBuf, String, Option<Vec<Lang>>, Lang, bool);

/// The fingerprint the editor keys its base-game cache by, for the install at `dir`.
pub(crate) fn vanilla_fingerprint(
    dir: &std::path::Path,
    ruleset: &RuleSet,
    shadow: Option<&cwtools_file_manager::file_manager::LayerShadow>,
) -> String {
    shadow.map_or_else(
        || cwtools_info::vanilla_cache::combined_fingerprint(dir, ruleset),
        |shadow| cwtools_driver::replacement_aware_vanilla_fingerprint(dir, ruleset, shadow),
    )
}

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
        // Merged even when empty, so a rebuild that finds no members drops the old build's.
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
        self.stage_vanilla_payload_transaction(data, None, || {})
    }

    fn stage_vanilla_payload_transaction<F: FnOnce()>(
        &self,
        data: cwtools_info::vanilla_cache::VanillaCacheData,
        rules_shape: Option<&str>,
        after_payload_staged: F,
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
        let mut vanilla = self.state.vanilla_state.lock();
        vanilla.publication_revision = vanilla.publication_revision.wrapping_add(1);
        *self.state.last_scan_fingerprint.lock() = None;
        vanilla.index = Some(per_type);
        if !loc_keys.is_empty() {
            vanilla.loc_keys = Some(loc_keys);
        }
        vanilla.file_paths = Some(Arc::new(file_paths));
        vanilla.var_names = Some(Arc::new(var_names));
        vanilla.scripted_loc_names = Some(Arc::new(scripted_loc_names));
        vanilla.scripted_gui_names = Some(Arc::new(scripted_gui_names));
        after_payload_staged();
        vanilla.rules_shape = rules_shape.map(str::to_owned);
        self.merge_vanilla_dynamic_values(complex_enum_values, value_set_values);
        drop(vanilla);
        total
    }

    /// Stage a payload built under `rules_shape`, so a later scan can tell a reload made it stale.
    fn stage_vanilla_payload_under(
        &self,
        data: cwtools_info::vanilla_cache::VanillaCacheData,
        rules_shape: &str,
    ) -> usize {
        self.stage_vanilla_payload_transaction(data, Some(rules_shape), || {})
    }

    /// Whether the held base-game data exists and was not built under another ruleset shape.
    fn base_game_is_current(&self, ruleset: Option<&RuleSet>) -> bool {
        let built_shape = {
            let vanilla = self.state.vanilla_state.lock();
            if vanilla.index.is_none() && !self.state.vanilla_merged.load(Ordering::SeqCst) {
                return false;
            }
            vanilla.rules_shape.clone()
        };
        match (built_shape, ruleset) {
            (Some(built), Some(ruleset)) => {
                built == cwtools_info::vanilla_cache::ruleset_shape_hash(ruleset)
            }
            _ => true,
        }
    }

    pub(crate) fn merge_pending_vanilla_index(&self) -> Result<(), FileError> {
        self.merge_pending_vanilla_index_versioned().map(|_| ())
    }

    pub(crate) fn merge_pending_vanilla_index_versioned(&self) -> Result<u64, FileError> {
        self.merge_pending_vanilla_index_with_revision(
            |workspace_root, parent_roots, ignore_files, ignore_dirs, vanilla_paths, shadow| {
                cwtools_driver::build_file_index_with_vanilla_shadow(
                    workspace_root,
                    parent_roots,
                    ignore_files,
                    ignore_dirs,
                    cwtools_driver::VanillaFiles::Cached(Cow::Borrowed(vanilla_paths)),
                    false,
                    shadow,
                )
            },
        )
    }

    #[cfg(test)]
    pub(super) fn merge_pending_vanilla_index_with<F>(
        &self,
        build_file_index: F,
    ) -> Result<(), FileError>
    where
        F: FnOnce(
            &std::path::Path,
            &[std::path::PathBuf],
            &[String],
            &[String],
            &[String],
            Option<&cwtools_file_manager::file_manager::LayerShadow>,
        ) -> Result<cwtools_info::FileIndex, FileError>,
    {
        self.merge_pending_vanilla_index_with_revision(build_file_index)
            .map(|_| ())
    }

    pub(super) fn merge_pending_vanilla_index_with_revision<F>(
        &self,
        build_file_index: F,
    ) -> Result<u64, FileError>
    where
        F: FnOnce(
            &std::path::Path,
            &[std::path::PathBuf],
            &[String],
            &[String],
            &[String],
            Option<&cwtools_file_manager::file_manager::LayerShadow>,
        ) -> Result<cwtools_info::FileIndex, FileError>,
    {
        let settings_generation = self.state.settings_generation.load(Ordering::SeqCst);
        let (
            var_names,
            scripted_loc_names,
            scripted_gui_names,
            file_paths,
            attempt_revision,
            publication_revision,
        ) = {
            // Snapshot the auxiliary payload without taking its index. An older
            // walk must not consume or publish it after a newer stage or settings change.
            let mut vanilla = self.state.vanilla_state.lock();
            vanilla.merge_attempt_revision = vanilla.merge_attempt_revision.wrapping_add(1);
            (
                vanilla.var_names.clone(),
                vanilla.scripted_loc_names.clone(),
                vanilla.scripted_gui_names.clone(),
                vanilla.file_paths.clone(),
                vanilla.merge_attempt_revision,
                vanilla.publication_revision,
            )
        };
        let (
            vanilla_dir,
            workspace_roots,
            parent_roots,
            ignore_files,
            ignore_dirs,
            replacement_shadow,
        ) = {
            let config = self.state.config.read();
            (
                config.vanilla_dir.clone(),
                config.workspace_roots.clone(),
                config.parent_roots.clone(),
                config.ignore_file_patterns.clone(),
                config.ignore_dir_patterns.clone(),
                config.workspace_roots.first().map(|primary| {
                    cwtools_driver::vanilla_replacement_shadow(primary, &config.parent_roots)
                }),
            )
        };
        // Build the fallible auxiliary index before consuming or publishing the staged
        // payload. A failed walk must leave it available for a later scan to retry.
        let config = workspace_roots.first().cloned().map(|workspace_root| {
            (
                workspace_root,
                parent_roots.clone(),
                ignore_files.clone(),
                ignore_dirs.clone(),
            )
        });
        let file_index =
            if let (Some((workspace_root, parent_roots, ignore_files, ignore_dirs)), Some(paths)) =
                (config, file_paths.as_ref())
            {
                match build_file_index(
                    &workspace_root,
                    &parent_roots,
                    &ignore_files,
                    &ignore_dirs,
                    paths,
                    replacement_shadow.as_ref(),
                ) {
                    Ok(index) => Some(index),
                    Err(error) => {
                        // A staged payload gets a fresh Arc, even if its paths are equal.
                        // Keep this lock through invalidation so a newer stage cannot be
                        // installed between the identity check and the state update.
                        let vanilla = self.state.vanilla_state.lock();
                        let same_payload = vanilla
                            .file_paths
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, paths));
                        let same_settings = self.state.settings_generation.load(Ordering::SeqCst)
                            == settings_generation;
                        let same_publication = vanilla.publication_revision == publication_revision;
                        if same_payload && same_settings && same_publication {
                            let mut vanilla = vanilla;
                            vanilla.publication_revision =
                                vanilla.publication_revision.wrapping_add(1);
                            let mut info = self.state.info_service.write();
                            Arc::make_mut(&mut info.type_index).complete = false;
                            self.state.vanilla_merged.store(false, Ordering::SeqCst);
                        }
                        return Err(error);
                    }
                }
            } else {
                None
            };
        // Match validation's info -> config order. Taking config first and then
        // waiting for info can deadlock behind a queued config writer.
        let mut vanilla = self.state.vanilla_state.lock();
        let mut info = self.state.info_service.write();
        let config = self.state.config.read();
        let same_payload = match (file_paths.as_ref(), vanilla.file_paths.as_ref()) {
            (Some(old), Some(current)) => Arc::ptr_eq(old, current),
            (None, None) => true,
            _ => false,
        };
        let same_settings = self.state.settings_generation.load(Ordering::SeqCst)
            == settings_generation
            && config.vanilla_dir == vanilla_dir
            && config.workspace_roots == workspace_roots
            && config.parent_roots == parent_roots
            && config.ignore_file_patterns == ignore_files
            && config.ignore_dir_patterns == ignore_dirs;
        let same_attempt = vanilla.merge_attempt_revision == attempt_revision;
        let same_publication = vanilla.publication_revision == publication_revision;
        if !same_payload || !same_settings || !same_attempt || !same_publication {
            return Err(FileError::Io(std::io::Error::other(
                "base-game payload, settings, or merge attempt changed during file indexing; retry",
            )));
        }
        let per_type = vanilla.index.take();
        let merged_index = per_type.is_some();
        // No staged index with a retained file-path payload means the type half
        // was already merged. A later successful auxiliary retry restores its
        // completeness after a failed walk, even if no install is available.
        let retry_retained = !merged_index
            && file_index.is_some()
            && !self.state.vanilla_merged.load(Ordering::SeqCst);
        if let Some(per_type) = per_type {
            let per_type = match (&vanilla_dir, &replacement_shadow) {
                (Some(vanilla_root), Some(shadow)) => {
                    cwtools_driver::filter_vanilla_index(per_type, vanilla_root, shadow)
                }
                _ => per_type,
            };
            let vanilla_root = vanilla_dir
                .as_deref()
                .and_then(|root| std::fs::canonicalize(root).ok());
            // Convert raw source paths to `file://` URIs, as workspace files are keyed, so goto
            // lands in the real base-game file; paths outside the install root are dropped. The
            // old "<vanilla-cache>" sentinel silently fell back to the open document (#62).
            let distinct: HashSet<&Arc<str>> =
                per_type.values().flatten().map(|(path, _)| path).collect();
            let uri_cache = resolve_source_uris(&distinct, vanilla_root.as_deref());
            let mut converted: HashMap<String, Vec<(Arc<str>, cwtools_info::TypeInstance)>> =
                HashMap::with_capacity(per_type.len());
            for (type_name, instances) in per_type {
                let mut out = Vec::with_capacity(instances.len());
                for (path, inst) in instances {
                    let Some(Some(uri)) = uri_cache.get(&path) else {
                        continue;
                    };
                    out.push((Arc::clone(uri), inst));
                }
                converted.insert(type_name, out);
            }
            let uris: HashSet<Arc<str>> = uri_cache.into_values().flatten().collect();
            let old = std::mem::replace(&mut vanilla.merged_uris, uris);

            let type_index = Arc::make_mut(&mut info.type_index);
            type_index.remove_files(&old);
            type_index.merge_base_game_with_uris(converted);
            type_index.complete = true;
            self.state.vanilla_merged.store(true, Ordering::SeqCst);
        }

        // Base-game variables are a distinct provenance, so a re-merge replaces the previous set
        // and a mod file sharing a name does not strip the vanilla definition (#306).
        if let Some(var_names) = var_names {
            Arc::make_mut(&mut info.type_index)
                .var_index
                .set_vanilla_names(&var_names);
        }

        // Base-game scripted loc names, same provenance split: a loc command naming one
        // must resolve without the mod having to define it (#348).
        if let Some(names) = scripted_loc_names {
            Arc::make_mut(&mut info.type_index)
                .scripted_loc_index
                .set_vanilla_names(&names);
        }

        if let Some(names) = scripted_gui_names {
            Arc::make_mut(&mut info.type_index)
                .scripted_gui_index
                .set_vanilla_names(&names);
        }

        if let Some(file_index) = file_index {
            let type_index = Arc::make_mut(&mut info.type_index);
            type_index.file_index = file_index;
            if retry_retained {
                type_index.complete = true;
                self.state.vanilla_merged.store(true, Ordering::SeqCst);
            }
        }
        vanilla.publication_revision = vanilla.publication_revision.wrapping_add(1);
        let revision = vanilla.publication_revision;
        drop(config);
        drop(info);
        drop(vanilla);
        self.bump_info_revision();
        if merged_index && !self.state.scan_in_progress.load(Ordering::SeqCst) {
            self.rebuild_alias_key_index();
        }
        Ok(revision)
    }

    pub(crate) async fn ensure_vanilla_index(
        &self,
        progress: Option<&CommandProgress>,
        force_rebuild: bool,
        quiet: bool,
    ) -> Result<(), FileError> {
        self.ensure_vanilla_index_with_loader(progress, force_rebuild, quiet, None)
            .await
    }

    pub(crate) async fn ensure_vanilla_index_with_loader(
        &self,
        progress: Option<&CommandProgress>,
        force_rebuild: bool,
        quiet: bool,
        loader: Option<VanillaLoader>,
    ) -> Result<(), FileError> {
        let ruleset_opt = self.state.rules.read().ruleset.clone();
        if !force_rebuild && self.base_game_is_current(ruleset_opt.as_deref()) {
            return Ok(());
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
            _ => return Ok(()),
        };
        if !was_explicit {
            let mut cfg = self.state.config.write();
            cfg.vanilla_dir = Some(dir.clone());
            cfg.refresh_roots();
        }

        let ruleset = match ruleset_opt {
            Some(rs) => rs,
            None => {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        "Base-game dir set but no rules loaded yet; skipping vanilla index.",
                    )
                    .await;
                return Ok(());
            }
        };

        let rules_shape = cwtools_info::vanilla_cache::ruleset_shape_hash(&ruleset);
        let fingerprint = vanilla_fingerprint(&dir, &ruleset, replacement_shadow.as_ref());
        let cache_path = self.vanilla_cache_path(&game, &fingerprint);

        if !force_rebuild
            && let Some(cp) = &cache_path
            && !cp.exists()
        {
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("No base-game cache at {}; building one", cp.display()),
                )
                .await;
        }

        if !force_rebuild
            && let Some(cp) = &cache_path
            && cp.exists()
        {
            match cwtools_info::vanilla_cache::load(cp) {
                Ok((cache_game, cache_fp, data))
                    if cache_game == game && cache_fp == fingerprint =>
                {
                    let total = self.stage_vanilla_payload_under(data, &rules_shape);
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
                    return Ok(());
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
            let shadow = if has_vanilla_replacements {
                shadow.as_ref()
            } else {
                None
            };
            if let Some(loader) = loader {
                loader(
                    &index_dir,
                    &ruleset,
                    &table,
                    parse_cache_dir.as_deref(),
                    &cache_game,
                    shadow,
                )
            } else if let Some(shadow) = shadow {
                index_vanilla_dir_with_shadow(
                    &index_dir,
                    &ruleset,
                    &table,
                    parse_cache_dir.as_deref(),
                    &cache_game,
                    Some(shadow),
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
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                tracing::error!("spawn_blocking vanilla index failed: {}", error);
                return Err(FileError::Io(std::io::Error::other(format!(
                    "base-game indexing task failed: {error}"
                ))));
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

        let total = self.stage_vanilla_payload_under(
            cwtools_info::vanilla_cache::VanillaCacheData { per_type, aux },
            &rules_shape,
        );
        self.client
            .log_message(
                MessageType::INFO,
                format!("Indexed {} base-game instances.", total),
            )
            .await;
        Ok(())
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

/// From this many distinct source paths up, canonicalization runs on the rayon pool. The cutoff
/// matches the scan stat signature's (#1153).
const PARALLEL_CANONICALIZE_MIN_PATHS: usize = 512;

/// Map each source path to its `file://` URI, or `None` when it is missing, outside `root`, or
/// there is no `root`.
pub fn resolve_source_uris(
    paths: &HashSet<&Arc<str>>,
    root: Option<&std::path::Path>,
) -> HashMap<Arc<str>, Option<Arc<str>>> {
    if paths.len() >= PARALLEL_CANONICALIZE_MIN_PATHS {
        use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
        paths
            .par_iter()
            .map(|&path| (Arc::clone(path), source_uri(path, root)))
            .collect()
    } else {
        paths
            .iter()
            .map(|&path| (Arc::clone(path), source_uri(path, root)))
            .collect()
    }
}

pub fn source_uri(path: &str, root: Option<&std::path::Path>) -> Option<Arc<str>> {
    let root = root?;
    let canonical = std::fs::canonicalize(path).ok()?;
    canonical
        .starts_with(root)
        .then(|| Arc::from(path_to_uri(&canonical).as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use cwtools_info::vanilla_cache::{VanillaCacheAux, VanillaCacheData};
    use cwtools_info::{SourceLocation, TypeInstance};

    use crate::testing::test_backend;

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

        backend
            .ensure_vanilla_index(None, false, true)
            .await
            .unwrap();

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
        backend
            .ensure_vanilla_index(None, false, true)
            .await
            .unwrap();
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
        backend
            .ensure_vanilla_index(None, false, true)
            .await
            .unwrap();
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

    #[tokio::test(flavor = "multi_thread")]
    async fn changing_a_value_set_alias_recollects_vanilla_members() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let vanilla = tmp.path().join("vanilla");
        let scripts = vanilla.join("common/ideas");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("base.txt"), "set_flag = ready_flag\n").unwrap();
        {
            let mut config = backend.state.config.write();
            config.language = "hoi4".into();
            config.workspace_roots = vec![workspace];
            config.vanilla_dir = Some(vanilla);
            config.cache_dir = Some(tmp.path().join("cache"));
            config.refresh_roots();
        }
        let table = cwtools_string_table::string_table::StringTable::new();
        let rules = |ns: &str| {
            let src = format!("alias[effect:set_flag] = value_set[{ns}]");
            let parsed = cwtools_parser::parser::parse_string(&src, &table);
            cwtools_rules::rules_converter::ast_to_ruleset(&parsed, &table)
        };
        backend.set_ruleset(rules("country_flag"));
        scan_base_game(&backend).await;
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .value_set_values
                .contains("country_flag", "ready_flag")
        );

        backend.set_ruleset(rules("state_flag"));
        scan_base_game(&backend).await;
        let info = backend.state.info_service.read();
        assert!(
            !info
                .type_index
                .value_set_values
                .contains("country_flag", "ready_flag")
        );
        assert!(
            info.type_index
                .value_set_values
                .contains("state_flag", "ready_flag")
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
        backend.merge_pending_vanilla_index().unwrap();
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
    fn repeated_merge_without_newly_staged_index_retains_payload() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut config = backend.state.config.write();
            config.workspace_roots = vec![tmp.path().to_path_buf()];
            config.refresh_roots();
        }
        let mut data = vanilla_data(vec!["vanilla_var"]);
        data.aux.file_paths = vec!["common/things/kept.dds".to_string()];
        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index().unwrap();
        let retained_file_paths = || backend.state.vanilla_state.lock().file_paths.clone();
        let before = retained_file_paths().expect("payload retained after the first merge");

        // Nothing is staged this time: the merge must still re-install the retained
        // payload (replacing, not accumulating) and rebuild the file index from it,
        // and the payload must survive for yet another merge (#1154).
        backend.merge_pending_vanilla_index().unwrap();
        {
            let idx = backend.state.info_service.read();
            assert!(idx.type_index.var_index.contains("vanilla_var"));
            assert!(idx.type_index.file_index.contains("common/things/kept.dds"));
        }
        let after = retained_file_paths().expect("payload retained after the second merge");
        assert!(
            Arc::ptr_eq(&before, &after),
            "a merge must share the retained payload, not replace it with a copy"
        );
    }

    #[test]
    fn failed_old_merge_does_not_restore_a_cleared_payload() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        backend.state.config.write().workspace_roots = vec![tmp.path().to_path_buf()];
        backend.stage_vanilla_payload(vanilla_data(vec!["old_var"]));
        Arc::make_mut(&mut backend.state.info_service.write().type_index).complete = true;
        let result = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
            // Match clear_vanilla_state's payload and completion changes while the
            // older merge is inside its file-index walk.
            let mut vanilla = backend.state.vanilla_state.lock();
            vanilla.index = None;
            vanilla.file_paths = None;
            vanilla.var_names = None;
            vanilla.scripted_loc_names = None;
            vanilla.scripted_gui_names = None;
            backend.state.vanilla_merged.store(false, Ordering::SeqCst);
            drop(vanilla);
            Err(FileError::FileCountBudget(tmp.path().to_path_buf()))
        });
        assert!(result.is_err());
        let vanilla = backend.state.vanilla_state.lock();
        assert!(vanilla.index.is_none());
        assert!(vanilla.file_paths.is_none());
        drop(vanilla);
        assert!(backend.state.info_service.read().type_index.complete);
        assert!(!backend.state.vanilla_merged.load(Ordering::SeqCst));
    }

    #[test]
    fn failed_old_merge_does_not_clobber_a_new_successful_merge() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        backend.state.config.write().workspace_roots = vec![tmp.path().to_path_buf()];
        backend.stage_vanilla_payload(vanilla_data(vec!["old_var"]));
        let result = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
            let mut newer = vanilla_data(vec!["new_var"]);
            newer.aux.file_paths = vec!["gfx/new.dds".into()];
            backend.stage_vanilla_payload(newer);
            backend.merge_pending_vanilla_index().unwrap();
            Err(FileError::FileCountBudget(tmp.path().to_path_buf()))
        });
        assert!(result.is_err());
        assert!(backend.state.vanilla_state.lock().index.is_none());
        assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
        let info = backend.state.info_service.read();
        assert!(info.type_index.complete);
        assert!(info.type_index.var_index.contains("new_var"));
        assert!(!info.type_index.var_index.contains("old_var"));
        assert!(info.type_index.file_index.contains("gfx/new.dds"));
    }

    #[test]
    fn older_failure_does_not_invalidate_a_newer_same_payload_success() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        backend.state.config.write().workspace_roots = vec![tmp.path().to_path_buf()];
        let mut data = vanilla_data(vec!["retained_var"]);
        data.aux.file_paths = vec!["gfx/retained.dds".into()];
        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index().unwrap();
        let paths = backend
            .state
            .vanilla_state
            .lock()
            .file_paths
            .clone()
            .unwrap();
        let result = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
            // Publish the same retained payload without staging a fresh Arc.
            backend.merge_pending_vanilla_index().unwrap();
            Err(FileError::FileCountBudget(tmp.path().to_path_buf()))
        });
        assert!(matches!(result, Err(FileError::FileCountBudget(_))));
        assert!(Arc::ptr_eq(
            &paths,
            backend
                .state
                .vanilla_state
                .lock()
                .file_paths
                .as_ref()
                .unwrap()
        ));
        assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
        let info = backend.state.info_service.read();
        assert!(info.type_index.complete);
        assert!(info.type_index.var_index.contains("retained_var"));
        assert!(info.type_index.file_index.contains("gfx/retained.dds"));
    }

    #[test]
    fn older_success_does_not_overwrite_a_newer_same_payload_success() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        backend.state.config.write().workspace_roots = vec![tmp.path().to_path_buf()];
        let mut data = vanilla_data(vec!["retained_var"]);
        data.aux.file_paths = vec!["gfx/retained.dds".into()];
        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index().unwrap();
        let result = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
            backend.merge_pending_vanilla_index().unwrap();
            Ok(cwtools_info::FileIndex::new())
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("merge attempt changed during file indexing; retry")
        );
        assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
        let info = backend.state.info_service.read();
        assert!(info.type_index.complete);
        assert!(info.type_index.var_index.contains("retained_var"));
        assert!(info.type_index.file_index.contains("gfx/retained.dds"));
    }

    #[test]
    fn older_success_does_not_override_a_newer_same_payload_failure() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        backend.state.config.write().workspace_roots = vec![tmp.path().to_path_buf()];
        let mut data = vanilla_data(vec!["retained_var"]);
        data.aux.file_paths = vec!["gfx/retained.dds".into()];
        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index().unwrap();
        let result = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
            let newer = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
                Err(FileError::FileCountBudget(tmp.path().to_path_buf()))
            });
            assert!(matches!(newer, Err(FileError::FileCountBudget(_))));
            Ok(cwtools_info::FileIndex::new())
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("merge attempt changed during file indexing; retry")
        );
        assert!(!backend.state.vanilla_merged.load(Ordering::SeqCst));
        assert!(!backend.state.info_service.read().type_index.complete);
        backend.merge_pending_vanilla_index().unwrap();
        assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
        assert!(backend.state.info_service.read().type_index.complete);
    }

    #[test]
    fn successful_old_walk_does_not_overwrite_a_newer_merged_payload() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let vanilla_root = tmp.path().join("vanilla");
        std::fs::create_dir_all(&vanilla_root).unwrap();
        for name in ["old.txt", "new.txt"] {
            std::fs::write(vanilla_root.join(name), "").unwrap();
        }
        {
            let mut config = backend.state.config.write();
            config.workspace_roots = vec![tmp.path().to_path_buf()];
            config.vanilla_dir = Some(vanilla_root.clone());
        }
        let payload = |filename: &str, name: &str| {
            let mut data = vanilla_data(vec![name]);
            data.aux.file_paths = vec![format!("gfx/{name}.dds")];
            data.per_type.insert(
                "test_type".into(),
                vec![(
                    Arc::from(vanilla_root.join(filename).to_string_lossy().as_ref()),
                    TypeInstance {
                        name: name.to_string(),
                        location: SourceLocation {
                            line: 0,
                            col: 0,
                            end: (0, 1),
                        },
                        primary_loc_key: None,
                        required_loc_keys: Vec::new(),
                    },
                )],
            );
            data
        };
        backend.stage_vanilla_payload(payload("old.txt", "old"));
        let result = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
            backend.stage_vanilla_payload(payload("new.txt", "new"));
            backend.merge_pending_vanilla_index().unwrap();
            Ok(cwtools_info::FileIndex::new())
        });
        assert!(result.is_err(), "the stale walk must request a retry");
        assert!(backend.state.vanilla_state.lock().index.is_none());
        assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
        let info = backend.state.info_service.read();
        let instances = info.type_index.map().get("test_type").unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].1.name, "new");
        assert!(info.type_index.var_index.contains("new"));
        assert!(!info.type_index.var_index.contains("old"));
        assert!(info.type_index.file_index.contains("gfx/new.dds"));
        assert!(info.type_index.complete);
    }

    #[test]
    fn successful_walk_with_changed_settings_leaves_payload_for_retry() {
        for increment_generation in [false, true] {
            let backend = test_backend();
            let tmp = tempfile::tempdir().unwrap();
            backend.state.config.write().workspace_roots = vec![tmp.path().to_path_buf()];
            backend.stage_vanilla_payload(vanilla_data(vec!["pending_var"]));
            let result = backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
                if increment_generation {
                    backend
                        .state
                        .settings_generation
                        .fetch_add(1, Ordering::SeqCst);
                } else {
                    // A config write can precede its generation bump.
                    backend
                        .state
                        .config
                        .write()
                        .ignore_file_patterns
                        .push("*.tmp".into());
                }
                Ok(cwtools_info::FileIndex::new())
            });
            assert!(result.is_err(), "changed settings must request a retry");
            assert!(backend.state.vanilla_state.lock().index.is_some());
            assert!(
                !backend
                    .state
                    .info_service
                    .read()
                    .type_index
                    .var_index
                    .contains("pending_var")
            );
            backend.merge_pending_vanilla_index().unwrap();
            assert!(
                backend
                    .state
                    .info_service
                    .read()
                    .type_index
                    .var_index
                    .contains("pending_var")
            );
            assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn blocked_merge_does_not_hold_config_while_waiting_for_info() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        backend.state.config.write().workspace_roots = vec![tmp.path().to_path_buf()];
        backend.stage_vanilla_payload(vanilla_data(vec!["pending_var"]));
        let info_reader = backend.state.info_service.read();
        let (started, walk_finished) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let merge = scope.spawn(|| {
                backend.merge_pending_vanilla_index_with(|_, _, _, _, _, _| {
                    started.send(()).unwrap();
                    Ok(cwtools_info::FileIndex::new())
                })
            });
            let walk_completed = walk_finished
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut blocked_on_info = false;
            while walk_completed && std::time::Instant::now() < deadline {
                if backend.state.vanilla_state.try_lock().is_none() {
                    blocked_on_info = true;
                    break;
                }
                std::thread::yield_now();
            }
            let config_available = blocked_on_info && backend.state.config.try_write().is_some();
            // Always release the reader before joining, even on an assertion failure.
            drop(info_reader);
            let result = merge.join().unwrap();
            assert!(walk_completed, "file-index walk did not finish");
            assert!(blocked_on_info, "merge must reach its publication fence");
            assert!(
                config_available,
                "a waiting merge must not hold config read"
            );
            assert!(result.is_ok());
        });
    }

    #[test]
    fn overlapping_stages_keep_shape_and_dynamic_values_with_their_payload() {
        let backend = test_backend();
        let payload = |name: &str| {
            let mut data = vanilla_data(vec![name]);
            data.aux.file_paths = vec![format!("gfx/{name}.dds")];
            data.aux.complex_enum_values = vec![("stats".into(), vec![name.into()])];
            data.aux.value_set_values = vec![("flags".into(), vec![name.into()])];
            data
        };
        let first = payload("first");
        let second = payload("second");
        let (start_second, second_started) = std::sync::mpsc::channel();
        let (ack_second, second_acknowledged) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let backend_ref = &backend;
            scope.spawn(move || {
                second_started.recv().unwrap();
                ack_second.send(()).unwrap();
                backend_ref.stage_vanilla_payload_under(second, "second_shape");
            });
            backend.stage_vanilla_payload_transaction(first, Some("first_shape"), || {
                start_second.send(()).unwrap();
                second_acknowledged
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                assert!(
                    backend.state.vanilla_state.try_lock().is_none(),
                    "staging must retain its payload fence until shape and values are set"
                );
            });
        });
        let vanilla = backend.state.vanilla_state.lock();
        assert_eq!(vanilla.rules_shape.as_deref(), Some("second_shape"));
        assert_eq!(
            vanilla.file_paths.as_ref().unwrap().as_slice(),
            &["gfx/second.dds"]
        );
        assert_eq!(vanilla.var_names.as_ref().unwrap().as_slice(), &["second"]);
        drop(vanilla);
        let info = backend.state.info_service.read();
        assert!(
            info.type_index
                .complex_enum_values
                .contains("stats", "second")
        );
        assert!(
            !info
                .type_index
                .complex_enum_values
                .contains("stats", "first")
        );
        assert!(info.type_index.value_set_values.contains("flags", "second"));
        assert!(!info.type_index.value_set_values.contains("flags", "first"));
    }

    #[test]
    fn concurrent_stage_never_pairs_an_old_index_with_new_aux() {
        // The pending index and the auxiliary payload are read under ONE
        // vanilla_state acquisition (#283). A merge that split the read could pair
        // a stale index with a freshly staged auxiliary payload, so which vanilla
        // name set gets installed must always match which instance snapshot was
        // merged. The interleave window is scheduler-biased, so this detects the
        // defect probabilistically and can never fail against a single-read merge.
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let vanilla_root = tmp.path().join("vanilla");
        std::fs::create_dir_all(vanilla_root.join("loop")).unwrap();
        std::fs::write(vanilla_root.join("loop/a.txt"), "").unwrap();
        std::fs::write(vanilla_root.join("loop/b.txt"), "").unwrap();
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
        let payload = |path: &str, name: &str| {
            let mut data = vanilla_data(vec![name]);
            let absolute = vanilla_root.join(path);
            data.per_type.insert(
                "scripted_effect".to_string(),
                vec![(
                    Arc::from(absolute.to_string_lossy().as_ref()),
                    instance(name),
                )],
            );
            data
        };
        for _ in 0..64 {
            backend.stage_vanilla_payload(payload("loop/a.txt", "alpha_var"));
            let outcome = std::thread::scope(|scope| {
                let merge = scope.spawn(|| backend.merge_pending_vanilla_index());
                std::thread::yield_now();
                backend.stage_vanilla_payload(payload("loop/b.txt", "beta_var"));
                merge.join().unwrap()
            });
            if let Err(error) = outcome {
                assert!(
                    error
                        .to_string()
                        .contains("changed during file indexing; retry"),
                    "{error}"
                );
            }
            std::thread::yield_now();

            let names_installed = {
                let idx = backend.state.info_service.read();
                idx.type_index.var_index.contains("alpha_var")
            };
            let index_merged = {
                let vanilla = backend.state.vanilla_state.lock();
                vanilla.merged_uris.iter().any(|uri| uri.contains("a.txt"))
            };
            assert_eq!(
                names_installed, index_merged,
                "the installed vanilla names and the merged instance provenance must come from the same staged snapshot"
            );
            backend.merge_pending_vanilla_index().unwrap();
            let info = backend.state.info_service.read();
            assert!(info.type_index.var_index.contains("beta_var"));
            drop(info);
            assert!(
                backend
                    .state
                    .vanilla_state
                    .lock()
                    .merged_uris
                    .iter()
                    .any(|uri| uri.contains("b.txt"))
            );
        }
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
        backend.merge_pending_vanilla_index().unwrap();

        let info = backend.state.info_service.read();
        let entries = info.type_index.map().get("foo").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.name, "in_root");
        assert_eq!(entries[0].0.as_ref(), path_to_uri(&canonical_in_root));
    }

    struct SourceFixture {
        _tmp: tempfile::TempDir,
        root: std::path::PathBuf,
        cases: Vec<(Arc<str>, Option<Arc<str>>)>,
    }

    /// `count` distinct source paths, each with the URI the merge must give it. The kinds cycle:
    /// in the root behind a `..` hop, outside the root, missing, and a link in the root whose
    /// target is outside it (unix only: Windows symlinks need privileges). `root` is canonical.
    fn source_fixture(count: usize) -> SourceFixture {
        let tmp = tempfile::tempdir().unwrap();
        let raw_root = tmp.path().join("vanilla");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(raw_root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let root = std::fs::canonicalize(&raw_root).unwrap();
        let kinds = if cfg!(unix) { 4 } else { 3 };
        let cases = (0..count)
            .map(|i| {
                let name = format!("f_{i:05}.txt");
                let (path, expected) = match i % kinds {
                    0 => {
                        std::fs::write(raw_root.join(&name), "").unwrap();
                        let path = raw_root.join("sub").join("..").join(&name);
                        let uri = path_to_uri(&std::fs::canonicalize(&path).unwrap());
                        (path, Some(uri))
                    }
                    1 => {
                        let path = outside.join(&name);
                        std::fs::write(&path, "").unwrap();
                        (path, None)
                    }
                    2 => (raw_root.join(&name), None),
                    _ => {
                        let target = outside.join(&name);
                        std::fs::write(&target, "").unwrap();
                        let link = raw_root.join(&name);
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(&target, &link).unwrap();
                        (link, None)
                    }
                };
                (
                    Arc::from(path.to_string_lossy().as_ref()),
                    expected.map(|uri| Arc::from(uri.as_str())),
                )
            })
            .collect();
        SourceFixture {
            _tmp: tmp,
            root,
            cases,
        }
    }

    #[test]
    fn source_uris_agree_on_both_sides_of_the_parallel_threshold() {
        for count in [
            PARALLEL_CANONICALIZE_MIN_PATHS - 1,
            PARALLEL_CANONICALIZE_MIN_PATHS,
        ] {
            let SourceFixture { _tmp, root, cases } = source_fixture(count);
            let expected: HashMap<Arc<str>, Option<Arc<str>>> = cases.into_iter().collect();
            let paths: HashSet<&Arc<str>> = expected.keys().collect();
            assert_eq!(paths.len(), count);

            let resolved = resolve_source_uris(&paths, Some(&root));
            let serial: HashMap<Arc<str>, Option<Arc<str>>> = paths
                .iter()
                .map(|&path| (Arc::clone(path), source_uri(path, Some(&root))))
                .collect();
            assert_eq!(resolved, expected, "{count} paths");
            assert_eq!(resolved, serial, "{count} paths");
        }
    }

    #[test]
    fn source_uris_without_a_root_drop_every_path() {
        let SourceFixture { _tmp, cases, .. } = source_fixture(PARALLEL_CANONICALIZE_MIN_PATHS);
        let paths: HashSet<&Arc<str>> = cases.iter().map(|(path, _)| path).collect();
        let resolved = resolve_source_uris(&paths, None);
        assert_eq!(resolved.len(), PARALLEL_CANONICALIZE_MIN_PATHS);
        assert!(resolved.values().all(Option::is_none));
    }

    #[test]
    fn merge_keeps_instance_order_above_the_parallel_threshold() {
        let backend = test_backend();
        let fixture = source_fixture(PARALLEL_CANONICALIZE_MIN_PATHS + 8);
        {
            let mut config = backend.state.config.write();
            config.vanilla_dir = Some(fixture.root.clone());
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
        let name = |i: usize| format!("inst_{i:05}");
        let mut instances: Vec<_> = fixture
            .cases
            .iter()
            .enumerate()
            .map(|(i, (path, _))| (Arc::clone(path), instance(&name(i))))
            .collect();
        let mut expected: Vec<(String, Arc<str>)> = fixture
            .cases
            .iter()
            .enumerate()
            .filter_map(|(i, (_, uri))| uri.as_ref().map(|uri| (name(i), Arc::clone(uri))))
            .collect();
        // A second instance in the first (in-root) file is kept, not collapsed with its path.
        let (first_path, first_uri) = &fixture.cases[0];
        instances.push((Arc::clone(first_path), instance("repeat")));
        expected.push((
            "repeat".to_string(),
            Arc::clone(first_uri.as_ref().expect("the first case is in the root")),
        ));
        let mut data = vanilla_data(Vec::new());
        data.per_type.insert("foo".to_string(), instances);
        backend.stage_vanilla_payload(data);
        backend.merge_pending_vanilla_index().unwrap();

        let info = backend.state.info_service.read();
        let merged: Vec<(String, Arc<str>)> = info
            .type_index
            .map()
            .get("foo")
            .unwrap()
            .iter()
            .map(|(uri, inst)| (inst.name.clone(), Arc::clone(uri)))
            .collect();
        assert_eq!(merged, expected);
        let uris: HashSet<Arc<str>> = expected.into_iter().map(|(_, uri)| uri).collect();
        assert_eq!(backend.state.vanilla_state.lock().merged_uris, uris);
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
        backend.merge_pending_vanilla_index().unwrap();

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
        backend.merge_pending_vanilla_index().unwrap();
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
        backend.merge_pending_vanilla_index().unwrap();

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
        backend.merge_pending_vanilla_index().unwrap();
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
        backend.merge_pending_vanilla_index().unwrap();
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
        backend.merge_pending_vanilla_index().unwrap();
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
        backend.merge_pending_vanilla_index().unwrap();
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
        backend.merge_pending_vanilla_index().unwrap();

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
        backend.merge_pending_vanilla_index().unwrap();
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
        backend.merge_pending_vanilla_index().unwrap();
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
        backend.merge_pending_vanilla_index().unwrap();
        assert!(backend.state.vanilla_merged.load(Ordering::SeqCst));
    }

    #[test]
    fn vanilla_vars_case_insensitive_via_vanilla_provenance() {
        let backend = test_backend();
        backend.stage_vanilla_payload(vanilla_data(vec!["VANILLA_VAR"]));
        backend.merge_pending_vanilla_index().unwrap();
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

    #[test]
    fn restaging_without_dynamic_values_removes_the_previous_ones() {
        let backend = test_backend();
        let mut old = vanilla_data(Vec::new());
        old.aux.complex_enum_values = vec![("stats".into(), vec!["OLD_STAT".into()])];
        old.aux.value_set_values = vec![("country_flag".into(), vec!["old_flag".into()])];
        backend.stage_vanilla_payload(old);
        {
            let info = backend.state.info_service.read();
            assert!(
                info.type_index
                    .complex_enum_values
                    .contains("stats", "OLD_STAT")
            );
            assert!(
                info.type_index
                    .value_set_values
                    .contains("country_flag", "old_flag")
            );
        }

        backend.stage_vanilla_payload(vanilla_data(Vec::new()));

        let info = backend.state.info_service.read();
        assert!(info.type_index.complex_enum_values.is_empty());
        assert!(info.type_index.value_set_values.is_empty());
    }

    fn ruleset_from(cwt: &str) -> RuleSet {
        let table = cwtools_string_table::string_table::StringTable::new();
        let parsed = cwtools_parser::parser::parse_string(cwt, &table);
        cwtools_rules::rules_converter::ast_to_ruleset(&parsed, &table)
    }

    /// Rules whose complex enum `stats` reads the member names under `common/<dir>`.
    fn stats_rules(dir: &str) -> RuleSet {
        ruleset_from(&format!(
            "enums = {{ complex_enum[stats] = {{ path = \"game/common/{dir}\" \
             start_from_root = yes name = {{ enum_name = scalar }} }} }}"
        ))
    }

    /// A backend with an installed base game holding `VALUE_ALPHA` under `common/alpha`
    /// and `VALUE_BETA` under `common/beta`, and a cache directory of its own.
    fn base_game_backend(root: &std::path::Path) -> Backend {
        let backend = test_backend();
        let vanilla_root = root.join("vanilla");
        for (dir, member) in [("alpha", "VALUE_ALPHA"), ("beta", "VALUE_BETA")] {
            let file = vanilla_root.join("common").join(dir).join("x.txt");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, format!("{member} = 1\n")).unwrap();
        }
        {
            let mut config = backend.state.config.write();
            config.language = "hoi4".into();
            config.vanilla_dir = Some(vanilla_root);
            config.cache_dir = Some(root.join("cache"));
            config.refresh_roots();
        }
        backend
    }

    async fn scan_base_game(backend: &Backend) {
        backend
            .ensure_vanilla_index(None, false, true)
            .await
            .unwrap();
        backend.merge_pending_vanilla_index().unwrap();
    }

    fn stats_members(backend: &Backend) -> Vec<String> {
        let info = backend.state.info_service.read();
        let mut members: Vec<String> = info
            .type_index
            .complex_enum_values
            .values("stats")
            .map(str::to_string)
            .collect();
        members.sort_unstable();
        members
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rules_reload_rebuilds_the_base_game_only_when_its_shape_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = base_game_backend(tmp.path());
        backend.set_ruleset(stats_rules("alpha"));
        scan_base_game(&backend).await;
        assert_eq!(stats_members(&backend), ["VALUE_ALPHA"]);

        backend.set_ruleset(stats_rules("alpha"));
        backend
            .ensure_vanilla_index(None, false, true)
            .await
            .unwrap();
        assert!(
            backend.state.vanilla_state.lock().index.is_none(),
            "a reload that keeps the shape must keep the merged base game"
        );
        assert_eq!(stats_members(&backend), ["VALUE_ALPHA"]);

        backend.set_ruleset(stats_rules("beta"));
        scan_base_game(&backend).await;
        assert_eq!(stats_members(&backend), ["VALUE_BETA"]);

        backend.set_ruleset(ruleset_from(""));
        scan_base_game(&backend).await;
        assert!(
            stats_members(&backend).is_empty(),
            "a removed complex enum must not leave the old build's members behind"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn base_game_of_unknown_origin_survives_a_rules_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = base_game_backend(tmp.path());
        backend.stage_vanilla_payload(vanilla_data(vec!["supplied_var"]));

        backend.set_ruleset(stats_rules("alpha"));
        backend
            .ensure_vanilla_index(None, false, true)
            .await
            .unwrap();

        assert!(
            stats_members(&backend).is_empty(),
            "a client-supplied cache has no known shape and must not be rebuilt"
        );
    }
}
