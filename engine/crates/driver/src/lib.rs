//! Load-and-validate pipeline shared by the CLI and the LSP. `Session` is the CLI's
//! batch path; the LSP keeps its own incremental state and calls the helpers here.

pub mod parent_mods;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod diagnostic_hash;

use cwtools_cache::workspace::{self as workspace_cache, SourceCacheKey};
use cwtools_file_manager::file_manager::{
    DirectoryType, DiscoveredFile, DiscoveredPath, DiscoveryReport, FileError, FileKind,
    FileManager, FileManagerConfig, LayerShadow, ScanBudget, ScanBytes, classify_directory,
    discover_paths, discover_paths_multi_mod, read_root_descriptor, read_text_reserved,
};
use cwtools_file_manager::read_text;
use cwtools_game::constants::Game;
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_index::vanilla_cache::{self, VanillaCacheData};
use cwtools_index::{
    CollectedVariablesAndValueSets, FileIndex, TypeIndex, collect_type_instances_with_subtypes,
    collect_variables_and_value_sets, index_discovered_files, variable_defining_effects,
};
use cwtools_localization::{Lang, LocDiagnostic, LocIndex, LocService};
use cwtools_parser::ast::{ParseError, ParsedFile};
use cwtools_parser::parser::parse_string_without_comments;
use cwtools_rules::rules_types::RuleSet;
use cwtools_rules::ruleset_loader::{
    RuleParseError, load_ruleset_from_dir, load_ruleset_from_file, load_ruleset_from_file_typed,
};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::inline_ignore::extract_inline_ignored_codes;
use cwtools_validation::references::{UsedInstances, check_unused_instances, needs_use_tracking};
use cwtools_validation::{
    ErrorSeverity, InlineScripts, Prepared, ValidationError, build_modifier_keys,
    build_scope_registry_arc, checks_from_env, validate_prepared, validate_prepared_tracking_uses,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryPolicy {
    Workspace,
    Vanilla,
}

pub struct WorkspaceDiscoveryRequest {
    pub roots: Vec<PathBuf>,
    pub kinds: Vec<FileKind>,
    pub config: FileManagerConfig,
    pub policy: DiscoveryPolicy,
}

pub struct SourceFile {
    pub path: PathBuf,
    pub logical_path: String,
    fingerprint: SourceFingerprint,
}

#[derive(Clone, PartialEq, Eq)]
struct SourceFingerprint {
    metadata: Option<SourceCacheKey>,
    content_hash: Option<u64>,
    source_size: u64,
}

struct ParsedFileSource {
    path: PathBuf,
    logical_path: String,
    parsed: ParsedFile,
    fingerprint: SourceFingerprint,
}

struct LoadedParse {
    parsed: ParsedFile,
    fingerprint: SourceFingerprint,
}

struct ParseCache {
    dir: PathBuf,
    fingerprint: u64,
    wrote: AtomicBool,
}

struct ParseReadBudget<'a> {
    max_file_size: u64,
    max_scan_bytes: u64,
    bytes: &'a ScanBytes,
    incomplete: &'a AtomicBool,
}

#[derive(Clone, Copy)]
enum ParseCacheUse {
    Full,
    Index,
}

fn open_parse_cache(cache_dir: &Path, game: &str, root: &Path) -> Option<ParseCache> {
    let fingerprint = workspace_cache::settings_fingerprint(game, root);
    match workspace_cache::validate_or_clear(cache_dir, fingerprint) {
        Ok(_) => Some(ParseCache {
            dir: cache_dir.to_path_buf(),
            fingerprint,
            wrote: AtomicBool::new(false),
        }),
        Err(error) => {
            eprintln!(
                "warn: parse cache unavailable at {}: {}",
                cache_dir.display(),
                error
            );
            None
        }
    }
}

fn load_or_parse(
    path: &Path,
    table: &StringTable,
    cache: Option<&ParseCache>,
    cache_use: ParseCacheUse,
    read_budget: Option<&ParseReadBudget>,
    snapshot_size: Option<u64>,
) -> Result<Option<LoadedParse>, FileError> {
    // Charge the source size before either reading it or accepting a metadata
    // cache hit. A bounded cold read may not exceed this reservation if the
    // file grows while the scan is in progress.
    let reservation = if let Some(budget) = read_budget {
        let metadata = std::fs::metadata(path)?;
        let size = metadata.len();
        if size > budget.max_file_size {
            return Err(FileError::OverLimit {
                path: path.to_path_buf(),
                limit: budget.max_file_size,
            });
        }
        if !budget.bytes.try_reserve(size, budget.max_scan_bytes) {
            budget.incomplete.store(true, Ordering::Relaxed);
            eprintln!(
                "warn: skipping {}: scan byte budget exceeded",
                path.display()
            );
            return Ok(None);
        }
        Some((
            size,
            workspace_cache::source_cache_key_from_metadata(path, &metadata),
        ))
    } else if let Some(size) = snapshot_size {
        let metadata = std::fs::metadata(path)?;
        if metadata.len() != size {
            return Err(FileError::Io(std::io::Error::other(
                "source changed during scan",
            )));
        }
        Some((
            size,
            workspace_cache::source_cache_key_from_metadata(path, &metadata),
        ))
    } else {
        None
    };
    let source_size = reservation
        .as_ref()
        .map(|(size, _)| *size)
        .or_else(|| std::fs::metadata(path).ok().map(|metadata| metadata.len()))
        .unwrap_or(0);
    if let Some(cache) = cache {
        let cached = match cache_use {
            ParseCacheUse::Full => {
                workspace_cache::load_path(&cache.dir, cache.fingerprint, path, table)
                    .map(|(parsed, key, _)| (parsed, key))
            }
            ParseCacheUse::Index => {
                workspace_cache::load_path_for_index(&cache.dir, cache.fingerprint, path, table)
            }
        };
        if let Some((parsed, metadata)) = cached {
            if reservation
                .as_ref()
                .is_some_and(|(_, expected)| *expected != metadata)
            {
                return Err(FileError::Io(std::io::Error::other(
                    "source changed during scan",
                )));
            }
            return Ok(Some(LoadedParse {
                parsed,
                fingerprint: SourceFingerprint {
                    metadata: Some(metadata),
                    content_hash: None,
                    source_size,
                },
            }));
        }
    }

    let metadata = workspace_cache::source_cache_key(path);
    let text = if let Some((reserved, expected)) = reservation {
        let (text, read) = read_text_reserved(path, reserved)?;
        if read != reserved || workspace_cache::source_cache_key(path).as_ref() != Some(&expected) {
            return Err(FileError::Io(std::io::Error::other(
                "source changed during scan",
            )));
        }
        text
    } else {
        read_text(path)?
    };
    let use_content_cache = !workspace_cache::PATH_METADATA_CACHE_SUPPORTED || metadata.is_none();
    let cached = if use_content_cache {
        cache.and_then(|cache| match cache_use {
            ParseCacheUse::Full => {
                workspace_cache::load(&cache.dir, cache.fingerprint, &text, table)
            }
            ParseCacheUse::Index => {
                workspace_cache::load_for_index(&cache.dir, cache.fingerprint, &text, table)
            }
        })
    } else {
        None
    };
    let cache_hit = cached.is_some();
    let parsed = match cached {
        Some(parsed) => parsed,
        None => parse_string_without_comments(&text, table),
    };
    let metadata =
        metadata.filter(|key| workspace_cache::source_cache_key(path).as_ref() == Some(key));
    if let Some(cache) = cache
        && !cache_hit
    {
        if let Some(source_key) = metadata.as_ref()
            && workspace_cache::PATH_METADATA_CACHE_SUPPORTED
        {
            let source =
                workspace_cache::SourceMeta::new(&text, extract_inline_ignored_codes(&text));
            workspace_cache::store_path(
                &cache.dir,
                cache.fingerprint,
                path,
                source_key,
                &parsed,
                &source,
                table,
            );
            cache.wrote.store(true, Ordering::Relaxed);
        } else {
            workspace_cache::store(&cache.dir, cache.fingerprint, &text, &parsed, table);
            cache.wrote.store(true, Ordering::Relaxed);
        }
    }
    Ok(Some(LoadedParse {
        parsed,
        fingerprint: SourceFingerprint {
            metadata,
            content_hash: use_content_cache.then(|| workspace_cache::content_hash(&text)),
            source_size,
        },
    }))
}

fn parse_discovered_files(
    files: Vec<DiscoveredFile>,
    table: &StringTable,
    cache: Option<&ParseCache>,
    read_budget: &ParseReadBudget,
) -> Vec<ParsedFileSource> {
    use rayon::prelude::*;

    files
        .into_par_iter()
        .filter_map(|file| {
            match load_or_parse(
                &file.path,
                table,
                cache,
                ParseCacheUse::Full,
                Some(read_budget),
                None,
            ) {
                Ok(Some(loaded)) => Some(ParsedFileSource {
                    path: file.path,
                    logical_path: file.logical_path,
                    parsed: loaded.parsed,
                    fingerprint: loaded.fingerprint,
                }),
                Ok(None) => None,
                Err(error) => {
                    read_budget.incomplete.store(true, Ordering::Relaxed);
                    eprintln!("warn: skipping {}: {}", file.path.display(), error);
                    None
                }
            }
        })
        .collect()
}

fn parse_discovered_files_for_index(
    files: Vec<DiscoveredFile>,
    table: &StringTable,
    cache: Option<&ParseCache>,
    config: &FileManagerConfig,
) -> Result<Vec<cwtools_file_manager::file_manager::ParsedFile>, FileError> {
    use rayon::prelude::*;

    let bytes = ScanBytes::new();
    let exhausted = AtomicBool::new(false);
    let read_budget = ParseReadBudget {
        max_file_size: config.read_cap(),
        max_scan_bytes: config.scan_budget.max_bytes,
        bytes: &bytes,
        incomplete: &exhausted,
    };
    let parsed = files
        .into_par_iter()
        .filter_map(|file| {
            match load_or_parse(
                &file.path,
                table,
                cache,
                ParseCacheUse::Index,
                Some(&read_budget),
                None,
            ) {
                Ok(Some(loaded)) => {
                    let parsed = loaded.parsed;
                    Some(cwtools_file_manager::file_manager::ParsedFile {
                        path: file.path,
                        logical_path: file.logical_path,
                        arena: parsed.arena,
                        root_children: parsed.root_children,
                        errors: parsed.errors,
                    })
                }
                Ok(None) => None,
                Err(error) => {
                    exhausted.store(true, Ordering::Relaxed);
                    eprintln!("warn: skipping {}: {}", file.path.display(), error);
                    None
                }
            }
        })
        .collect();
    if exhausted.load(Ordering::Relaxed) {
        return Err(FileError::Io(std::io::Error::other(
            "base-game script acquisition incomplete",
        )));
    }
    Ok(parsed)
}

pub enum RulesInput {
    Dir(PathBuf),
    File(PathBuf),
}

impl RulesInput {
    pub fn from_path(path: PathBuf) -> Self {
        if path.is_dir() {
            RulesInput::Dir(path)
        } else {
            RulesInput::File(path)
        }
    }
}

/// # Examples
pub fn vanilla_gated_checks(game: Game, has_vanilla: bool) -> &'static [&'static str] {
    if has_vanilla {
        return &[];
    }
    match game {
        Game::Stellaris => &["CW113", "CW222", "CW227", "CW229", "CW250", "CW500"],
        _ => &["CW113", "CW222", "CW500"],
    }
}

/// Auto-managed on-disk cache of the base-game index, so a batch run does not
/// re-parse the whole install every time. Only used when `vanilla` is a directory
/// and no explicit `vanilla_cache` was supplied.
#[derive(Debug, Clone)]
pub struct VanillaCacheAuto {
    /// Directory holding one cache file per game + fingerprint. Same layout and
    /// filename as the LSP's, so the CLI and the editor share warm caches.
    pub dir: PathBuf,
    pub refresh: bool,
}

pub struct SessionConfig<'a> {
    pub game: Game,
    pub rules: RulesInput,
    pub directory: PathBuf,
    /// Higher-priority script/localisation layers between the workspace and
    /// vanilla game. Parents are indexed for references but never validated.
    pub parent_roots: Vec<PathBuf>,
    pub vanilla: Option<PathBuf>,
    pub vanilla_cache: Option<VanillaCacheData>,
    pub vanilla_cache_auto: Option<VanillaCacheAuto>,
    pub ignore_files: &'a [String],
    pub ignore_dirs: &'a [String],
    pub loc_languages: Option<Vec<Lang>>,
    /// Match CW113 `filepath` references to the mod's own files by exact case. Off by
    /// default for Windows-authored mods; enable for mods that also target Linux/Mac,
    /// so a reference that differs from the file only by case is flagged.
    pub case_sensitive_files: bool,
    pub on_rules_diagnostic: Option<&'a mut dyn FnMut(RuleParseError)>,
}

pub struct Session {
    game: Game,
    rules_table: StringTable,
    ruleset: RuleSet,
    type_index: TypeIndex,
    modifier_keys: HashSet<String>,
    inline_scripts: InlineScripts,
    loc_service: LocService,
    loc_index: LocIndex,
    loc_languages: Option<Vec<Lang>>,
    registry: Option<Arc<ScopeRegistry>>,
    directory: PathBuf,
    parse_cache: Option<ParseCache>,
}

impl Session {
    pub fn load(config: SessionConfig) -> SessionWithFiles {
        Self::load_with_parse_cache(config, None)
    }

    pub fn load_with_parse_cache(
        config: SessionConfig,
        parse_cache_dir: Option<PathBuf>,
    ) -> SessionWithFiles {
        let SessionConfig {
            game,
            rules,
            directory,
            parent_roots,
            vanilla,
            mut vanilla_cache,
            vanilla_cache_auto,
            ignore_files,
            ignore_dirs,
            loc_languages,
            case_sensitive_files,
            on_rules_diagnostic,
        } = config;
        let vanilla_shadow = vanilla_replacement_shadow(&directory, &parent_roots);

        let rules_table = StringTable::new();
        let mut rules_acquisition_failed = false;
        let (ruleset, rule_errors) = match &rules {
            RulesInput::Dir(dir) => load_ruleset_from_dir(dir, &rules_table, ScanBudget::default()),
            RulesInput::File(file) => {
                load_ruleset_from_file_typed(file, &rules_table, ScanBudget::default())
                    .unwrap_or_else(|e| {
                        rules_acquisition_failed = e.retryable_io;
                        eprintln!("error: {}", e);
                        (RuleSet::new(), Vec::new())
                    })
            }
        };
        if let Some(sink) = on_rules_diagnostic {
            for error in rule_errors {
                sink(error);
            }
        }
        let game_id = game.to_string();
        let parse_cache = parse_cache_dir
            .as_deref()
            .and_then(|dir| open_parse_cache(dir, &game_id, &directory));
        let vanilla_parse_cache_dir = vanilla_cache_auto
            .as_ref()
            .and_then(|_| parse_cache.as_ref().map(|cache| cache.dir.clone()));

        let mut fm_config = workspace_discovery_config(&directory, Some(&ruleset));
        if !ignore_files.is_empty() {
            fm_config
                .exclude_patterns
                .extend(ignore_files.iter().cloned());
        }
        if !ignore_dirs.is_empty() {
            fm_config
                .exclude_dir_patterns
                .extend(ignore_dirs.iter().cloned());
        }

        let script_bytes = ScanBytes::new();
        let script_incomplete = AtomicBool::new(false);
        let script_read_budget = ParseReadBudget {
            max_file_size: fm_config.read_cap(),
            max_scan_bytes: fm_config.scan_budget.max_bytes,
            bytes: &script_bytes,
            incomplete: &script_incomplete,
        };

        let discovery = discover_workspace(WorkspaceDiscoveryRequest {
            roots: vec![directory.clone()],
            kinds: vec![FileKind::Script, FileKind::Localisation],
            config: fm_config,
            policy: DiscoveryPolicy::Workspace,
        });
        let mut discovery_incomplete = false;
        let (files, loc_paths, primary_scripts, primary_loc_paths, discovery_failed) =
            match discovery {
                Ok(discovery) => {
                    discovery_incomplete = !discovery.failures.is_empty();
                    for failure in discovery.failures {
                        eprintln!(
                            "warn: discovery skipped {}: {}",
                            failure.path.display(),
                            failure.error
                        );
                    }
                    let primary_scripts: Vec<DiscoveredFile> = discovery
                        .files
                        .iter()
                        .filter(|file| file.kind == FileKind::Script)
                        .map(|file| DiscoveredFile {
                            path: file.path.clone(),
                            logical_path: file.root_relative_path.clone(),
                        })
                        .collect();
                    let primary_loc_paths: Vec<DiscoveredPath> = discovery
                        .files
                        .iter()
                        .filter(|file| file.kind == FileKind::Localisation)
                        .cloned()
                        .collect();
                    let loc_paths = primary_loc_paths
                        .iter()
                        .map(|file| file.path.clone())
                        .collect();
                    let files = parse_discovered_files(
                        primary_scripts.clone(),
                        &rules_table,
                        parse_cache.as_ref(),
                        &script_read_budget,
                    );
                    (files, loc_paths, primary_scripts, primary_loc_paths, false)
                }
                Err(e) => {
                    eprintln!("error: discovery failed for {}: {}", directory.display(), e);
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new(), true)
                }
            };
        let mut parent_status = ParentDiscoveryStatus::default();
        let parent_scripts = discover_parent_script_files_inner(
            &parent_roots,
            &directory,
            &primary_scripts,
            Some(&ruleset),
            ignore_files,
            ignore_dirs,
            &mut parent_status,
        );
        let parsed_parents = parse_discovered_files(
            parent_scripts,
            &rules_table,
            parse_cache.as_ref(),
            &script_read_budget,
        );
        discovery_incomplete |= script_incomplete.load(Ordering::Relaxed);
        let parent_loc_paths = discover_parent_localisation_files_inner(
            &parent_roots,
            &directory,
            &primary_loc_paths,
            ignore_files,
            ignore_dirs,
            &mut parent_status,
        );
        discovery_incomplete |= parent_status.incomplete;
        if let Some(cache) = &parse_cache
            && cache.wrote.swap(false, Ordering::Relaxed)
        {
            workspace_cache::prune(&cache.dir, cache.fingerprint);
        }

        let mut parsed = files;
        let primary_file_count = parsed.len();
        parsed.extend(parsed_parents);

        use rayon::prelude::*;
        type PerFileResult = (
            HashMap<String, Vec<cwtools_index::TypeInstance>>,
            HashMap<String, Vec<cwtools_index::TypeInstance>>,
            cwtools_index::DerivedNames,
            Vec<String>,
            HashMap<String, Vec<String>>,
            HashMap<String, Vec<String>>,
            Vec<String>,
            Vec<String>,
        );
        let var_effects = variable_defining_effects(&ruleset);
        let per_file: Vec<PerFileResult> = parsed
            .par_iter()
            .map(|src| {
                let collected = collect_type_instances_with_subtypes(
                    &ruleset,
                    &src.parsed,
                    &src.logical_path,
                    &rules_table,
                    cwtools_validation::subtype_membership_for_instance,
                );
                let CollectedVariablesAndValueSets {
                    variables,
                    value_sets,
                } = collect_variables_and_value_sets(
                    &src.parsed,
                    &ruleset,
                    &rules_table,
                    Some(&var_effects),
                );
                let var_names = variables.into_iter().map(|d| d.name).collect();
                let complex = cwtools_index::dynamic_values::collect_complex_enum_values(
                    &ruleset,
                    &src.parsed,
                    &src.logical_path,
                    &rules_table,
                );
                let scripted_locs = cwtools_index::collect_scripted_loc_names(
                    &src.parsed,
                    &src.logical_path,
                    &rules_table,
                );
                let scripted_guis = cwtools_index::collect_scripted_gui_callback_names(
                    &src.parsed,
                    &src.logical_path,
                    &rules_table,
                );
                (
                    collected.instances,
                    collected.subtype_instances,
                    collected.derived,
                    var_names,
                    value_sets,
                    complex,
                    scripted_locs,
                    scripted_guis,
                )
            })
            .collect();

        let mut type_index = TypeIndex::new();
        for (
            (file_index, src),
            (
                instances,
                subtype_instances,
                derived,
                var_names,
                value_sets,
                complex,
                scripted_locs,
                scripted_guis,
            ),
        ) in parsed.iter().enumerate().zip(per_file)
        {
            let file_uri = cwtools_file_manager::path_identity(&src.path);
            let is_parent = file_index >= primary_file_count;
            if is_parent {
                type_index.merge_base_game(&file_uri, instances);
            } else {
                type_index.merge(&file_uri, instances);
            }
            if !subtype_instances.is_empty() {
                if is_parent {
                    type_index.merge_base_game(&file_uri, subtype_instances);
                } else {
                    type_index.merge(&file_uri, subtype_instances);
                }
            }
            type_index.merge_derived(&file_uri, derived);
            for n in &var_names {
                type_index.var_index.add_name(n);
            }
            type_index
                .value_set_values
                .merge_file(&file_uri, value_sets);
            type_index
                .complex_enum_values
                .merge_file(&file_uri, complex);
            type_index
                .scripted_loc_index
                .merge_file(&file_uri, scripted_locs);
            type_index
                .scripted_gui_index
                .merge_file(&file_uri, scripted_guis);
        }
        let mut source_files: Vec<SourceFile> = parsed
            .into_iter()
            .map(|src| SourceFile {
                path: src.path,
                logical_path: src.logical_path,
                fingerprint: src.fingerprint,
            })
            .collect();
        // Parents sit after the primary files; they supply inline scripts but
        // are not validated.
        let (inline_scripts, inline_incomplete) = load_inline_scripts(&source_files, &rules_table);
        discovery_incomplete |= inline_incomplete;
        source_files.truncate(primary_file_count);

        let mut cache_write_target: Option<(PathBuf, String)> = None;
        let mut force_vanilla_rebuild = false;
        if let (None, Some(auto), Some(vanilla_dir)) =
            (&vanilla_cache, &vanilla_cache_auto, &vanilla)
        {
            let fingerprint =
                replacement_aware_vanilla_fingerprint(vanilla_dir, &ruleset, &vanilla_shadow);
            let path = vanilla_cache_path(&auto.dir, &game_id, &fingerprint);
            if !auto.refresh {
                vanilla_cache = load_fresh_vanilla_cache(&path, &game_id, &fingerprint);
            }
            if vanilla_cache.is_none() {
                cache_write_target = Some((path, fingerprint));
                force_vanilla_rebuild = auto.refresh;
            }
        }

        let missing_vanilla = vanilla.as_ref().filter(|dir| !dir.is_dir()).cloned();
        let mut vanilla_discovery_failed = false;
        let mut has_vanilla_data = vanilla_cache.is_some();
        let mut cached_loc_keys: Option<Vec<(String, Vec<String>)>> = None;
        if let Some(cache) = vanilla_cache {
            if let Some(vanilla_dir) = &missing_vanilla {
                vanilla_discovery_failed = true;
                eprintln!(
                    "error: discovery failed for vanilla {}: {}",
                    vanilla_dir.display(),
                    FileError::MissingRoot(vanilla_dir.clone())
                );
            }
            let vanilla_per_type = if let Some(vanilla_root) = vanilla.as_deref() {
                filter_vanilla_index(cache.per_type, vanilla_root, &vanilla_shadow)
            } else {
                cache.per_type
            };
            type_index.merge_base_game_with_uris(vanilla_per_type);
            let aux = cache.aux;
            for n in &aux.var_names {
                type_index.var_index.add_name(n);
            }
            match build_file_index_with_vanilla_shadow(
                &directory,
                &parent_roots,
                ignore_files,
                ignore_dirs,
                VanillaFiles::Cached(Cow::Owned(aux.file_paths)),
                case_sensitive_files,
                Some(&vanilla_shadow),
            ) {
                Ok(index) => type_index.file_index = index,
                Err(error) => {
                    eprintln!("error: file index discovery incomplete: {error}");
                    discovery_incomplete = true;
                    vanilla_discovery_failed = true;
                }
            }
            type_index.complex_enum_values.merge_file(
                "<vanilla-cache>",
                aux.complex_enum_values.into_iter().collect(),
            );
            type_index.value_set_values.merge_file(
                "<vanilla-cache>",
                aux.value_set_values.into_iter().collect(),
            );
            type_index
                .scripted_loc_index
                .set_vanilla_names(&aux.scripted_loc_names);
            type_index
                .scripted_gui_index
                .set_vanilla_names(&aux.scripted_gui_names);
            cached_loc_keys = Some(aux.loc_keys);
        } else if let Some(vanilla_dir) = &vanilla {
            let vanilla_index = if let Some(missing_vanilla) = &missing_vanilla {
                Err(FileError::MissingRoot(missing_vanilla.clone()))
            } else if let Some(parse_cache_dir) = &vanilla_parse_cache_dir
                && !force_vanilla_rebuild
            {
                index_game_dir_with_parse_cache_and_shadow(
                    vanilla_dir,
                    &ruleset,
                    &rules_table,
                    &var_effects,
                    parse_cache_dir,
                    &game_id,
                    &vanilla_shadow,
                )
            } else {
                index_game_dir_with_shadow(
                    vanilla_dir,
                    &ruleset,
                    &rules_table,
                    &var_effects,
                    &vanilla_shadow,
                )
            };
            match vanilla_index {
                Ok(vanilla_index) => {
                    let aux = build_vanilla_cache_aux_with_shadow(
                        vanilla_dir,
                        &vanilla_index,
                        &vanilla_shadow,
                    );
                    match &aux {
                        Ok(_) => has_vanilla_data = true,
                        Err(error) => {
                            vanilla_discovery_failed = true;
                            eprintln!("error: base-game auxiliary discovery incomplete: {error}");
                        }
                    }
                    if let Some((path, fingerprint)) = &cache_write_target {
                        match write_vanilla_cache_checked(
                            &vanilla_index,
                            &game_id,
                            fingerprint,
                            path,
                            aux,
                        ) {
                            Ok(n) => eprintln!(
                                "  Cached {} base-game instances to {} ({})",
                                n,
                                path.display(),
                                fingerprint
                            ),
                            Err(e) => eprintln!(
                                "  warn: could not write base-game cache {}: {}",
                                path.display(),
                                e
                            ),
                        }
                    }
                    type_index.var_index.merge(&vanilla_index.var_index);
                    type_index.complex_enum_values.merge_file(
                        "<vanilla-dynamic>",
                        vanilla_index
                            .complex_enum_values
                            .export()
                            .into_iter()
                            .collect(),
                    );
                    type_index.value_set_values.merge_file(
                        "<vanilla-dynamic>",
                        vanilla_index
                            .value_set_values
                            .export()
                            .into_iter()
                            .collect(),
                    );
                    let scripted_loc_names: Vec<String> = vanilla_index
                        .scripted_loc_index
                        .names()
                        .map(str::to_string)
                        .collect();
                    type_index
                        .scripted_loc_index
                        .set_vanilla_names(&scripted_loc_names);
                    let scripted_gui_names: Vec<String> = vanilla_index
                        .scripted_gui_index
                        .names()
                        .map(str::to_string)
                        .collect();
                    type_index
                        .scripted_gui_index
                        .set_vanilla_names(&scripted_gui_names);
                    let vanilla_per_type = filter_vanilla_index(
                        vanilla_index.into_map(),
                        vanilla_dir,
                        &vanilla_shadow,
                    );
                    type_index.merge_base_game_with_uris(vanilla_per_type);
                    match build_file_index_with_vanilla_shadow(
                        &directory,
                        &parent_roots,
                        ignore_files,
                        ignore_dirs,
                        VanillaFiles::Install(vanilla_dir),
                        case_sensitive_files,
                        Some(&vanilla_shadow),
                    ) {
                        Ok(index) => type_index.file_index = index,
                        Err(error) => {
                            eprintln!("error: file index discovery incomplete: {error}");
                            discovery_incomplete = true;
                            vanilla_discovery_failed = true;
                        }
                    }
                }
                Err(error) => {
                    vanilla_discovery_failed = true;
                    eprintln!(
                        "error: discovery failed for vanilla {}: {}",
                        vanilla_dir.display(),
                        error
                    );
                }
            }
        }

        let modifier_keys = build_modifier_keys(&ruleset, &type_index);

        let mut loc_service =
            LocService::from_paths(loc_paths, ScanBudget::default(), loc_languages.as_deref());
        let mut loc_index = LocIndex::build_scoped(&loc_service, loc_languages.as_deref());
        let parent_loc_service = LocService::from_paths(
            parent_loc_paths.into_iter().map(|file| file.path).collect(),
            ScanBudget::default(),
            loc_languages.as_deref(),
        );
        let parent_loc_index =
            LocIndex::build_scoped(&parent_loc_service, loc_languages.as_deref());
        loc_index.merge_from(&parent_loc_index, loc_languages.as_deref());
        if cached_loc_keys.is_none()
            && let Some(v) = &vanilla
        {
            let vanilla_paths: Vec<_> = match localisation_paths(
                std::slice::from_ref(v),
                &[],
                &[],
                DiscoveryPolicy::Vanilla,
            ) {
                Ok(paths) => paths
                    .into_iter()
                    .filter(|path| !vanilla_shadow.hides_file_under(v, path))
                    .collect(),
                Err(error) => {
                    eprintln!("error: base-game localisation discovery incomplete: {error}");
                    vanilla_discovery_failed = true;
                    Vec::new()
                }
            };
            let vanilla_loc = LocService::from_paths(
                vanilla_paths,
                ScanBudget::default(),
                loc_languages.as_deref(),
            );
            let vanilla_index = LocIndex::build_scoped(&vanilla_loc, None);
            loc_index.merge_from(&vanilla_index, loc_languages.as_deref());
            loc_service.merge_from(vanilla_loc);
        }
        if let Some(keys) = cached_loc_keys {
            let typed: Vec<(Lang, Vec<String>)> = keys
                .into_iter()
                .filter_map(|(name, ks)| Lang::from_name(&name).map(|l| (l, ks)))
                .filter(|(lang, _)| loc_languages.as_ref().is_none_or(|ls| ls.contains(lang)))
                .collect();
            loc_index.merge_cached_keys(typed, loc_languages.as_deref());
        }

        if has_vanilla_data && !vanilla_discovery_failed {
            type_index.complete = true;
        }

        let registry = build_scope_registry_arc(&ruleset, Some(game));

        Session {
            game,
            rules_table,
            ruleset,
            type_index,
            modifier_keys,
            inline_scripts,
            loc_service,
            loc_index,
            loc_languages,
            registry,
            directory,
            parse_cache,
        }
        .with_source_files(
            source_files,
            discovery_failed || vanilla_discovery_failed,
            discovery_incomplete || rules_acquisition_failed,
        )
    }

    fn with_source_files(
        self,
        files: Vec<SourceFile>,
        discovery_failed: bool,
        discovery_incomplete: bool,
    ) -> SessionWithFiles {
        SessionWithFiles {
            session: self,
            files,
            discovery_failed,
            discovery_incomplete,
            validation_incomplete: AtomicBool::new(false),
        }
    }

    fn prepared(&self) -> Prepared<'_> {
        let (scope_checks, var_checks) = checks_from_env();
        Prepared {
            ruleset: &self.ruleset,
            table: &self.rules_table,
            game: Some(self.game),
            type_index: Some(&self.type_index),
            modifier_keys: Some(&self.modifier_keys),
            loc_index: Some(&self.loc_index),
            extra_loc_keys: None,
            inline_scripts: Some(&self.inline_scripts),
            registry: self.registry.as_ref(),
            scope_checks,
            var_checks,
        }
    }

    pub fn loc_project_diagnostics(&self) -> Vec<LocDiagnostic> {
        let extra = self.loc_extra_valid_refs();
        cwtools_localization::validate_loc_project_with_union(
            &self.loc_service,
            self.loc_languages.as_deref(),
            self.loc_index.union(),
            &extra,
        )
    }

    pub fn loc_extra_valid_refs(&self) -> HashSet<String> {
        let mut extra = self.modifier_keys.clone();
        extra.extend(self.type_index.loc_bindable_names());
        extra
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn string_table(&self) -> &StringTable {
        &self.rules_table
    }

    pub fn ruleset(&self) -> &RuleSet {
        &self.ruleset
    }

    pub fn type_index(&self) -> &TypeIndex {
        &self.type_index
    }

    pub fn modifier_keys(&self) -> &HashSet<String> {
        &self.modifier_keys
    }

    pub fn inline_scripts(&self) -> &InlineScripts {
        &self.inline_scripts
    }

    pub fn loc_index(&self) -> &LocIndex {
        &self.loc_index
    }

    pub fn registry(&self) -> Option<&Arc<ScopeRegistry>> {
        self.registry.as_ref()
    }
}

pub struct SessionWithFiles {
    session: Session,
    files: Vec<SourceFile>,
    pub discovery_failed: bool,
    /// A partial discovery walk failed; used by watch reloads.
    pub discovery_incomplete: bool,
    validation_incomplete: AtomicBool,
}

impl std::ops::Deref for SessionWithFiles {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}

impl SessionWithFiles {
    /// Whether discovery or later source acquisition left validation incomplete.
    pub fn is_incomplete(&self) -> bool {
        self.discovery_incomplete || self.validation_incomplete.load(Ordering::Relaxed)
    }

    /// Read the indexed source snapshot for report hashing and inline directives.
    /// A changed or unreadable source makes the validation pass incomplete.
    pub fn read_source_text(&self, source: &SourceFile) -> Result<String, FileError> {
        let result = read_source_snapshot(source);
        if result.is_err() {
            self.validation_incomplete.store(true, Ordering::Relaxed);
        }
        result
    }

    /// Read an indexed script for fix application without accepting changed
    /// content or converting non-UTF-8 scripts into UTF-8 on write.
    pub fn read_source_utf8(&self, source: &SourceFile) -> Result<String, FileError> {
        let bytes = match read_source_snapshot_bytes(source) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.validation_incomplete.store(true, Ordering::Relaxed);
                return Err(error);
            }
        };
        let text = String::from_utf8(bytes).map_err(|error| {
            FileError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        if source
            .fingerprint
            .content_hash
            .is_some_and(|hash| workspace_cache::content_hash(&text) != hash)
        {
            self.validation_incomplete.store(true, Ordering::Relaxed);
            return Err(FileError::Io(std::io::Error::other(
                "source changed after indexing",
            )));
        }
        Ok(text)
    }

    pub fn validate_all(&self) -> Vec<(PathBuf, Vec<ValidationError>)> {
        self.validate_selected(None)
    }

    pub fn validate_selected(
        &self,
        only: Option<&HashSet<PathBuf>>,
    ) -> Vec<(PathBuf, Vec<ValidationError>)> {
        use rayon::prelude::*;

        let prepared = self.session.prepared();
        let track_uses =
            prepared.type_index.is_some() && needs_use_tracking(prepared.ruleset, prepared.game);
        let only = only.filter(|_| !track_uses);
        let mut results: Vec<(PathBuf, Vec<ValidationError>, UsedInstances)> = self
            .files
            .par_iter()
            .filter(|src| only.is_none_or(|wanted| wanted.contains(&src.path)))
            .map(|src| {
                let file_str: cwtools_validation::FilePath = std::sync::Arc::from(
                    cwtools_file_manager::path_identity(&src.path).into_owned(),
                );
                let file_display = src.path.display().to_string();
                let changed_error = || {
                    self.validation_incomplete.store(true, Ordering::Relaxed);
                    (
                        src.path.clone(),
                        vec![ValidationError {
                            message: format!(
                                "file changed after indexing: {file_display}; rerun validation"
                            ),
                            severity: ErrorSeverity::Error,
                            line: 0,
                            col: 0,
                            file: std::sync::Arc::clone(&file_str),
                            code: None,
                            fix: None,
                            end: None,
                            related: Vec::new(),
                        }],
                        UsedInstances::default(),
                    )
                };
                if workspace_cache::source_cache_key(&src.path) != src.fingerprint.metadata {
                    return changed_error();
                }
                let parsed = match load_or_parse(
                    &src.path,
                    &self.session.rules_table,
                    self.session.parse_cache.as_ref(),
                    ParseCacheUse::Full,
                    None,
                    Some(src.fingerprint.source_size),
                ) {
                    Ok(Some(loaded)) if loaded.fingerprint == src.fingerprint => loaded.parsed,
                    Ok(_) => return changed_error(),
                    Err(error) => {
                        self.validation_incomplete.store(true, Ordering::Relaxed);
                        return (
                            src.path.clone(),
                            vec![ValidationError {
                                message: format!("could not parse {file_display}: {error}"),
                                severity: ErrorSeverity::Error,
                                line: 0,
                                col: 0,
                                file: file_str,
                                code: None,
                                fix: None,
                                end: None,
                                related: Vec::new(),
                            }],
                            UsedInstances::default(),
                        );
                    }
                };
                let mut errors = parse_errors_to_validation(&parsed.errors, &file_str);
                let used = if track_uses {
                    let (errs, used) =
                        validate_prepared_tracking_uses(&parsed, &file_str, &prepared);
                    errors.extend(errs);
                    used
                } else {
                    errors.extend(validate_prepared(&parsed, &file_str, &prepared));
                    UsedInstances::default()
                };
                // CW100: objects defined here whose `## required` loc keys no loc file
                // provides. Gated on a non-empty loc index, or a mod with no
                // `localisation/` would report every key as missing.
                if let (Some(loc), Some(type_index)) = (prepared.loc_index, prepared.type_index)
                    && !loc.union().is_empty()
                {
                    let instances = type_index.instances_in_file(&file_str);
                    errors.extend(cwtools_validation::missing_loc::check_missing_localisation(
                        &instances,
                        &src.logical_path,
                        &file_str,
                        prepared.ruleset,
                        |k| loc.exists_any(k),
                    ));
                }
                (src.path.clone(), errors, used)
            })
            .collect();

        if track_uses && let Some(type_index) = prepared.type_index {
            let mut used = UsedInstances::default();
            for (_, _, file_used) in &mut results {
                used.absorb(std::mem::take(file_used));
            }
            for (path, errors, _) in &mut results {
                let file_str: cwtools_validation::FilePath =
                    std::sync::Arc::from(cwtools_file_manager::path_identity(path).into_owned());
                let instances = type_index.instances_in_file(&file_str);
                errors.extend(check_unused_instances(
                    prepared.ruleset,
                    prepared.game,
                    &instances,
                    &used,
                    &file_str,
                ));
            }
        }

        results
            .into_iter()
            .map(|(path, errors, _)| (path, errors))
            .collect()
    }

    pub fn parsed_files(&self) -> &[SourceFile] {
        &self.files
    }
}

fn read_source_snapshot_bytes(file: &SourceFile) -> Result<Vec<u8>, FileError> {
    use std::io::Read as _;
    if workspace_cache::source_cache_key(&file.path) != file.fingerprint.metadata {
        return Err(FileError::Io(std::io::Error::other(
            "source changed after indexing",
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&file.path)?
        .take(file.fingerprint.source_size)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != file.fingerprint.source_size
        || workspace_cache::source_cache_key(&file.path) != file.fingerprint.metadata
    {
        return Err(FileError::Io(std::io::Error::other(
            "source changed after indexing",
        )));
    }
    Ok(bytes)
}

fn read_source_snapshot(file: &SourceFile) -> Result<String, FileError> {
    let (text, _) =
        cwtools_file_manager::file_manager::decode_bytes(read_source_snapshot_bytes(file)?);
    if file
        .fingerprint
        .content_hash
        .is_some_and(|hash| workspace_cache::content_hash(&text) != hash)
    {
        return Err(FileError::Io(std::io::Error::other(
            "source changed after indexing",
        )));
    }
    Ok(text)
}

fn load_inline_scripts(files: &[SourceFile], table: &StringTable) -> (InlineScripts, bool) {
    let mut scripts = InlineScripts::default();
    let mut incomplete = false;
    for file in files
        .iter()
        .filter(|file| InlineScripts::is_script_path(&file.logical_path))
    {
        match read_source_snapshot(file) {
            Ok(text) => {
                scripts.insert(
                    &file.logical_path,
                    parse_string_without_comments(&text, table),
                );
            }
            Err(error) => {
                incomplete = true;
                eprintln!(
                    "warn: skipping inline script {}: {}",
                    file.path.display(),
                    error
                );
            }
        }
    }
    (scripts, incomplete)
}

fn parse_errors_to_validation(
    errors: &[ParseError],
    file_path: &cwtools_validation::FilePath,
) -> Vec<ValidationError> {
    errors
        .iter()
        .map(|ParseError::Pos(line, col, msg)| ValidationError {
            message: msg.clone(),
            severity: ErrorSeverity::Error,
            line: *line,
            col: *col,
            file: std::sync::Arc::clone(file_path),
            code: None,
            fix: None,
            end: None,
            related: Vec::new(),
        })
        .collect()
}

fn checked_discovery(report: DiscoveryReport) -> Result<DiscoveryReport, FileError> {
    if let Some(failure) = report.failures.first() {
        if failure.is_file_count_budget() {
            return Err(FileError::FileCountBudget(failure.path.clone()));
        }
        return Err(FileError::Io(std::io::Error::other(format!(
            "discovery skipped {}: {}",
            failure.path.display(),
            failure.error
        ))));
    }
    Ok(report)
}

fn localisation_paths_with_budget(
    roots: &[PathBuf],
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: DiscoveryPolicy,
    budget: ScanBudget,
) -> Result<Vec<PathBuf>, FileError> {
    let mut config = FileManagerConfig {
        scan_budget: budget,
        ..Default::default()
    };
    if policy == DiscoveryPolicy::Workspace {
        config.exclude_patterns.extend(ignore_files.iter().cloned());
        config
            .exclude_dir_patterns
            .extend(ignore_dirs.iter().cloned());
    }
    let report = checked_discovery(discover_workspace(WorkspaceDiscoveryRequest {
        roots: roots.to_vec(),
        kinds: vec![FileKind::Localisation],
        config,
        policy,
    })?)?;
    Ok(report.files.into_iter().map(|file| file.path).collect())
}

fn localisation_paths(
    roots: &[PathBuf],
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: DiscoveryPolicy,
) -> Result<Vec<PathBuf>, FileError> {
    localisation_paths_with_budget(
        roots,
        ignore_files,
        ignore_dirs,
        policy,
        ScanBudget::default(),
    )
}

pub fn load_loc_service(
    dirs: &[&Path],
    langs: Option<&[Lang]>,
    ignore_files: &[String],
    ignore_dirs: &[String],
) -> Result<LocService, FileError> {
    let roots: Vec<PathBuf> = dirs.iter().map(|dir| (*dir).to_path_buf()).collect();
    let paths = localisation_paths(
        &roots,
        ignore_files,
        ignore_dirs,
        DiscoveryPolicy::Workspace,
    )?;
    Ok(LocService::from_paths(paths, ScanBudget::default(), langs))
}

/// Cache file for `game` at `fingerprint` under `dir`. The name comes from the cache
/// module, so a cache warmed by either side is reused by the other and neither
/// drifts from what `clearAllCaches` recognizes.
fn vanilla_cache_path(dir: &Path, game: &str, fingerprint: &str) -> PathBuf {
    dir.join(vanilla_cache::cache_file_name(game, fingerprint))
}

/// Cache a replacement-filtered vanilla view separately from the unfiltered
/// install so loading a cache never mixes auxiliary indexes from two views.
pub fn replacement_aware_vanilla_fingerprint(
    dir: &Path,
    ruleset: &RuleSet,
    shadow: &LayerShadow,
) -> String {
    let fingerprint = vanilla_cache::combined_fingerprint(dir, ruleset);
    if shadow.has_replace_paths() {
        format!("{fingerprint}|{}", shadow.replacement_fingerprint())
    } else {
        fingerprint
    }
}

fn write_vanilla_cache(
    index: &TypeIndex,
    game: &str,
    fingerprint: &str,
    path: &Path,
    aux: cwtools_index::vanilla_cache::VanillaCacheAux,
) -> std::io::Result<usize> {
    let tmp = path.with_extension(format!("cwv.tmp{}", std::process::id()));
    let written = vanilla_cache::save(index, game, fingerprint, &tmp, aux)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(written),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn write_vanilla_cache_checked(
    index: &TypeIndex,
    game: &str,
    fingerprint: &str,
    path: &Path,
    aux: Result<cwtools_index::vanilla_cache::VanillaCacheAux, FileError>,
) -> Result<usize, FileError> {
    Ok(write_vanilla_cache(index, game, fingerprint, path, aux?)?)
}

/// Read a cached base-game index, but only if it was built for this game from this
/// exact install and ruleset. Anything else reads as a miss so the caller
/// re-indexes; a stale cache is never silently used.
fn load_fresh_vanilla_cache(
    path: &Path,
    game: &str,
    fingerprint: &str,
) -> Option<VanillaCacheData> {
    if !path.exists() {
        return None;
    }
    match vanilla_cache::load(path) {
        Ok((cache_game, cache_fp, data)) if cache_game == game && cache_fp == fingerprint => {
            let total: usize = data.per_type.values().map(|v| v.len()).sum();
            eprintln!(
                "  Loaded {} base-game instances from cache {} ({})",
                total,
                path.display(),
                fingerprint
            );
            Some(data)
        }
        Ok((cache_game, cache_fp, _)) => {
            eprintln!(
                "  Base-game cache {} is stale (cached {}/{}, install {}/{}); re-indexing",
                path.display(),
                cache_game,
                cache_fp,
                game,
                fingerprint
            );
            None
        }
        Err(e) => {
            eprintln!(
                "  warn: could not read base-game cache {}: {}",
                path.display(),
                e
            );
            None
        }
    }
}

/// `XDG_CACHE_HOME`/`LOCALAPPDATA`, else `~/.cache` (Linux) or `~/Library/Caches`
pub fn default_cache_dir() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME")
        && !x.is_empty()
    {
        return Some(PathBuf::from(x).join("cwtools"));
    }
    if let Ok(la) = std::env::var("LOCALAPPDATA")
        && !la.is_empty()
    {
        return Some(PathBuf::from(la).join("cwtools"));
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        {
            return Some(home.join("Library/Caches/cwtools"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Some(home.join(".cache/cwtools"));
        }
    }
    Some(std::env::temp_dir().join("cwtools"))
}

pub enum VanillaFiles<'a> {
    /// Owned moves into the index; borrowed spares a retaining caller a copy (#1154).
    Cached(Cow<'a, [String]>),
    Install(&'a Path),
}

/// Build the index a `filepath` reference (CW113) resolves against. Both front ends build it
/// here, so the editor and the CLI answer the same reference the same way (#283).
/// `parent_roots` are parent mods (#786): a submod may reference any file they ship.
pub fn build_file_index_with_vanilla_shadow(
    workspace_root: &Path,
    parent_roots: &[PathBuf],
    workspace_ignore_files: &[String],
    workspace_ignore_dirs: &[String],
    vanilla: VanillaFiles<'_>,
    case_sensitive: bool,
    shadow: Option<&LayerShadow>,
) -> Result<FileIndex, FileError> {
    let mut index = FileIndex::new();
    index.set_case_sensitive(case_sensitive);
    let workspace_paths = discover_file_index_paths(
        workspace_root,
        workspace_ignore_files,
        workspace_ignore_dirs,
        DiscoveryPolicy::Workspace,
    )?;
    // The workspace claims its own paths so a watched delete knows what to drop (#1067).
    for path in &workspace_paths {
        index.claim_workspace(path);
    }
    // No same-path shadowing by the workspace: a parent file it overrides must still be
    // seen to survive the override's delete. `replace_path` still hides (#1067).
    let mut parent_error = None;
    let parent_paths = visible_parent_files(
        parent_roots,
        workspace_root,
        &[],
        |root| match discover_file_index_paths(
            root,
            workspace_ignore_files,
            workspace_ignore_dirs,
            DiscoveryPolicy::Workspace,
        ) {
            Ok(paths) => paths,
            Err(error) => {
                parent_error.get_or_insert(error);
                Vec::new()
            }
        },
        String::as_str,
    );
    if let Some(error) = parent_error {
        return Err(error);
    }
    index.add_paths(parent_paths);
    match vanilla {
        VanillaFiles::Cached(Cow::Owned(paths)) => index.add_paths(
            paths
                .into_iter()
                .filter(|path| !shadow.is_some_and(|shadow| shadow.hides(path))),
        ),
        VanillaFiles::Cached(Cow::Borrowed(paths)) => index.add_paths(
            paths
                .iter()
                .filter(|path| !shadow.is_some_and(|shadow| shadow.hides(path)))
                .map(String::as_str),
        ),
        VanillaFiles::Install(dir) => {
            let paths = discover_file_index_paths(dir, &[], &[], DiscoveryPolicy::Vanilla)?;
            index.add_paths(
                paths
                    .into_iter()
                    .filter(|path| !shadow.is_some_and(|shadow| shadow.hides(path))),
            );
        }
    }
    Ok(index)
}

fn discover_file_index_paths_with_budget(
    root: &Path,
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: DiscoveryPolicy,
    budget: ScanBudget,
) -> Result<Vec<String>, FileError> {
    let mut config = FileManagerConfig {
        include_dirs: vec![".".to_string()],
        scan_budget: budget,
        ..Default::default()
    };
    if policy == DiscoveryPolicy::Workspace {
        config.exclude_patterns.extend(ignore_files.iter().cloned());
        config
            .exclude_dir_patterns
            .extend(ignore_dirs.iter().cloned());
    }
    let report = checked_discovery(discover_workspace(WorkspaceDiscoveryRequest {
        roots: vec![root.to_path_buf()],
        kinds: vec![FileKind::Script, FileKind::Localisation, FileKind::Resource],
        config,
        policy,
    })?)?;
    Ok(report
        .files
        .into_iter()
        .map(|file| file.root_relative_path)
        .collect())
}

fn discover_file_index_paths(
    root: &Path,
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: DiscoveryPolicy,
) -> Result<Vec<String>, FileError> {
    discover_file_index_paths_with_budget(
        root,
        ignore_files,
        ignore_dirs,
        policy,
        ScanBudget::default(),
    )
}

pub fn build_vanilla_cache_aux(
    vanilla_dir: &Path,
    index: &TypeIndex,
) -> Result<cwtools_index::vanilla_cache::VanillaCacheAux, FileError> {
    build_vanilla_cache_aux_with_optional_shadow(vanilla_dir, index, None)
}

pub fn build_vanilla_cache_aux_with_shadow(
    vanilla_dir: &Path,
    index: &TypeIndex,
    shadow: &LayerShadow,
) -> Result<cwtools_index::vanilla_cache::VanillaCacheAux, FileError> {
    build_vanilla_cache_aux_with_optional_shadow(vanilla_dir, index, Some(shadow))
}

fn build_vanilla_cache_aux_with_optional_shadow(
    vanilla_dir: &Path,
    index: &TypeIndex,
    shadow: Option<&LayerShadow>,
) -> Result<cwtools_index::vanilla_cache::VanillaCacheAux, FileError> {
    build_vanilla_cache_aux_with_budget(vanilla_dir, index, shadow, ScanBudget::default())
}

fn build_vanilla_cache_aux_with_budget(
    vanilla_dir: &Path,
    index: &TypeIndex,
    shadow: Option<&LayerShadow>,
    budget: ScanBudget,
) -> Result<cwtools_index::vanilla_cache::VanillaCacheAux, FileError> {
    let loc_paths: Vec<_> = localisation_paths_with_budget(
        &[vanilla_dir.to_path_buf()],
        &[],
        &[],
        DiscoveryPolicy::Vanilla,
        budget,
    )?
    .into_iter()
    .filter(|path| !shadow.is_some_and(|shadow| shadow.hides_file_under(vanilla_dir, path)))
    .collect();
    let loc_service = LocService::from_paths(loc_paths, ScanBudget::default(), None);
    let loc_keys = cwtools_localization::loc_index::per_language_keys(&loc_service);
    let mut file_index = cwtools_index::FileIndex::new();
    // Preserve disk spelling so a case-sensitive run can check vanilla references.
    file_index.set_case_sensitive(true);
    let file_paths = discover_file_index_paths_with_budget(
        vanilla_dir,
        &[],
        &[],
        DiscoveryPolicy::Vanilla,
        budget,
    )?
    .into_iter()
    .filter(|path| !shadow.is_some_and(|shadow| shadow.hides(path)));
    file_index.add_paths(file_paths);
    Ok(cwtools_index::vanilla_cache::VanillaCacheAux {
        loc_keys,
        file_paths: file_index.paths_exact().cloned().collect(),
        var_names: index.var_index.names().cloned().collect(),
        complex_enum_values: index.complex_enum_values.export(),
        value_set_values: index.value_set_values.export(),
        scripted_loc_names: index
            .scripted_loc_index
            .names()
            .map(str::to_string)
            .collect(),
        scripted_gui_names: index
            .scripted_gui_index
            .names()
            .map(str::to_string)
            .collect(),
    })
}

pub fn index_game_dir(
    dir: &Path,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: &HashSet<String>,
) -> Result<TypeIndex, FileError> {
    index_game_dir_with_cache_and_shadow(dir, ruleset, table, var_effects, None, None)
}

pub fn index_game_dir_with_shadow(
    dir: &Path,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: &HashSet<String>,
    shadow: &LayerShadow,
) -> Result<TypeIndex, FileError> {
    index_game_dir_with_cache_and_shadow(dir, ruleset, table, var_effects, None, Some(shadow))
}

pub fn index_game_dir_with_parse_cache(
    dir: &Path,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: &HashSet<String>,
    cache_dir: &Path,
    game: &str,
) -> Result<TypeIndex, FileError> {
    let cache = open_parse_cache(cache_dir, game, dir);
    index_game_dir_with_cache_and_shadow(dir, ruleset, table, var_effects, cache.as_ref(), None)
}

pub fn index_game_dir_with_parse_cache_and_shadow(
    dir: &Path,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: &HashSet<String>,
    cache_dir: &Path,
    game: &str,
    shadow: &LayerShadow,
) -> Result<TypeIndex, FileError> {
    let cache = open_parse_cache(cache_dir, game, dir);
    index_game_dir_with_cache_and_shadow(
        dir,
        ruleset,
        table,
        var_effects,
        cache.as_ref(),
        Some(shadow),
    )
}

fn index_game_dir_with_cache_and_shadow(
    dir: &Path,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: &HashSet<String>,
    cache: Option<&ParseCache>,
    shadow: Option<&LayerShadow>,
) -> Result<TypeIndex, FileError> {
    let mut config = search_config_for(dir);
    apply_config_folders(&mut config, &ruleset.folders);
    let mgr = FileManager::with_string_table(config, table.clone());
    let discovered = mgr.discover_files()?;
    let discovered: Vec<_> = discovered
        .into_iter()
        .filter(|file| !shadow.is_some_and(|shadow| shadow.hides(&file.logical_path)))
        .collect();
    let files = parse_discovered_files_for_index(discovered, table, cache, &mgr.config)?;
    if let Some(cache) = cache
        && cache.wrote.swap(false, Ordering::Relaxed)
    {
        workspace_cache::prune(&cache.dir, cache.fingerprint);
    }
    Ok(index_discovered_files(
        files,
        ruleset,
        table,
        Some(var_effects),
        Some(cwtools_validation::subtype_membership_for_instance),
    ))
}

pub(crate) fn apply_config_folders(config: &mut FileManagerConfig, folders: &[String]) {
    if folders.is_empty() {
        return;
    }
    if folders.iter().any(|f| config.root.join(f).is_dir()) {
        config.include_dirs = folders.to_vec();
    }
}

/// Build the `FileManagerConfig` a workspace scan should use, narrowed to the ruleset's
/// `folders`. A multi-mod workspace gets exactly those folders, since its root lacks the
/// game folders themselves. Shared so the CLI and the editor discover the same files (#284).
pub fn workspace_discovery_config(root: &Path, ruleset: Option<&RuleSet>) -> FileManagerConfig {
    let mut config = search_config_for(root);
    if let Some(ruleset) = ruleset {
        apply_config_folders(&mut config, &ruleset.folders);
        if classify_directory(root) == DirectoryType::MultipleMod && !ruleset.folders.is_empty() {
            config.include_dirs = ruleset.folders.clone();
        }
    }
    config
}

/// Discover the workspace's script files from a config built by
/// [`workspace_discovery_config`]. Handles single-mod and multi-mod workspaces, so the
/// CLI and LSP share one branching point (#284).
pub fn discover_workspace_files(
    config: FileManagerConfig,
) -> Result<Vec<DiscoveredFile>, FileError> {
    let (files, failures, _) = discover_workspace_files_with_failures(config)?;
    if let Some(failure) = failures
        .iter()
        .find(|failure| failure.is_file_count_budget())
    {
        return Err(FileError::FileCountBudget(failure.path.clone()));
    }
    Ok(files)
}

fn discover_workspace_files_with_failures(
    config: FileManagerConfig,
) -> Result<
    (
        Vec<DiscoveredFile>,
        Vec<cwtools_file_manager::file_manager::DiscoveryFailure>,
        usize,
    ),
    FileError,
> {
    let (kind, mut failures) =
        cwtools_file_manager::file_manager::classify_directory_with_failures(&config.root);
    let manager = FileManager::new(config);
    let (files, walk_failures, examined) = if kind == DirectoryType::MultipleMod {
        manager.discover_files_multi_mod_with_usage()
    } else {
        let (files, failures) = manager.discover_files_with_failures()?;
        let examined = files.len();
        (files, failures, examined)
    };
    failures.extend(walk_failures);
    Ok((files, failures, examined))
}

/// The parent-mod files a primary mod still sees (#786), in load order. Each
/// parent is walked from the last-loaded down, dropping what the primary or
/// a later parent shadows (see [`LayerShadow`]). `discover` lists one root's
/// candidate files and `logical_path` names a file's root-relative path, so
/// scripts and localisation share one layering rule.
pub fn visible_parent_files<T>(
    parent_roots: &[PathBuf],
    primary_root: &Path,
    primary_logical_paths: &[&str],
    mut discover: impl FnMut(&Path) -> Vec<T>,
    logical_path: impl Fn(&T) -> &str,
) -> Vec<T> {
    let replace_paths = |root: &Path| {
        read_root_descriptor(root)
            .map(|descriptor| descriptor.replace_paths)
            .unwrap_or_default()
    };
    let mut shadow = LayerShadow::default();
    shadow.claim(
        primary_logical_paths.iter().copied(),
        &replace_paths(primary_root),
    );
    let mut layers: Vec<Vec<T>> = Vec::with_capacity(parent_roots.len());
    for root in parent_roots.iter().rev() {
        let kept: Vec<T> = discover(root)
            .into_iter()
            .filter(|file| !shadow.hides(logical_path(file)))
            .collect();
        shadow.claim(kept.iter().map(&logical_path), &replace_paths(root));
        layers.push(kept);
    }
    layers.into_iter().rev().flatten().collect()
}

/// [`visible_parent_files`] over script files, each parent discovered the
/// way [`workspace_discovery_config`] discovers the primary.
pub fn discover_parent_script_files(
    parent_roots: &[PathBuf],
    primary_root: &Path,
    primary_files: &[DiscoveredFile],
    ruleset: Option<&RuleSet>,
    ignore_files: &[String],
    ignore_dirs: &[String],
) -> Result<Vec<DiscoveredFile>, FileError> {
    let mut status = ParentDiscoveryStatus::default();
    let files = discover_parent_script_files_inner(
        parent_roots,
        primary_root,
        primary_files,
        ruleset,
        ignore_files,
        ignore_dirs,
        &mut status,
    );
    parent_discovery_result(files, status.incomplete)
}

struct ParentDiscoveryStatus {
    incomplete: bool,
    budget: ScanBudget,
    remaining: usize,
}

impl ParentDiscoveryStatus {
    fn new(budget: ScanBudget) -> Self {
        Self {
            incomplete: false,
            remaining: budget.files_remaining(),
            budget,
        }
    }

    fn next_budget(&self) -> ScanBudget {
        ScanBudget {
            max_files: if self.budget.max_files == 0 {
                0
            } else {
                self.remaining.max(1)
            },
            ..self.budget
        }
    }

    fn record(&mut self, examined: usize, failed: bool) -> bool {
        let omitted = self.budget.max_files != 0 && self.remaining == 0 && examined > 0;
        self.incomplete |= failed || omitted;
        self.remaining = self.remaining.saturating_sub(examined);
        !omitted
    }
}

impl Default for ParentDiscoveryStatus {
    fn default() -> Self {
        Self::new(ScanBudget::default())
    }
}

fn parent_discovery_result<T>(files: Vec<T>, incomplete: bool) -> Result<Vec<T>, FileError> {
    if incomplete {
        Err(FileError::Io(std::io::Error::other(
            "parent discovery incomplete",
        )))
    } else {
        Ok(files)
    }
}

fn discover_parent_script_files_inner(
    parent_roots: &[PathBuf],
    primary_root: &Path,
    primary_files: &[DiscoveredFile],
    ruleset: Option<&RuleSet>,
    ignore_files: &[String],
    ignore_dirs: &[String],
    status: &mut ParentDiscoveryStatus,
) -> Vec<DiscoveredFile> {
    let primary: Vec<&str> = primary_files
        .iter()
        .map(|file| file.logical_path.as_str())
        .collect();
    visible_parent_files(
        parent_roots,
        primary_root,
        &primary,
        |root| {
            let mut config = workspace_discovery_config(root, ruleset);
            config.scan_budget = status.next_budget();
            config.exclude_patterns.extend(ignore_files.iter().cloned());
            config
                .exclude_dir_patterns
                .extend(ignore_dirs.iter().cloned());
            discover_workspace_files_with_failures(config)
                .map(|(files, failures, examined)| {
                    if status.record(examined, !failures.is_empty()) {
                        files
                    } else {
                        Vec::new()
                    }
                })
                .unwrap_or_else(|error| {
                    status.incomplete = true;
                    eprintln!(
                        "warn: parent mod discovery failed for {}: {error}",
                        root.display()
                    );
                    Vec::new()
                })
        },
        |file| file.logical_path.as_str(),
    )
}

/// [`visible_parent_files`] over localisation files.
pub fn discover_parent_localisation_files(
    parent_roots: &[PathBuf],
    primary_root: &Path,
    primary_files: &[DiscoveredPath],
    ignore_files: &[String],
    ignore_dirs: &[String],
) -> Result<Vec<DiscoveredPath>, FileError> {
    discover_parent_localisation_files_with_budget(
        parent_roots,
        primary_root,
        primary_files,
        ignore_files,
        ignore_dirs,
        ScanBudget::default(),
    )
}

pub fn discover_parent_localisation_files_with_budget(
    parent_roots: &[PathBuf],
    primary_root: &Path,
    primary_files: &[DiscoveredPath],
    ignore_files: &[String],
    ignore_dirs: &[String],
    budget: ScanBudget,
) -> Result<Vec<DiscoveredPath>, FileError> {
    let mut status = ParentDiscoveryStatus::new(budget);
    let files = discover_parent_localisation_files_inner(
        parent_roots,
        primary_root,
        primary_files,
        ignore_files,
        ignore_dirs,
        &mut status,
    );
    parent_discovery_result(files, status.incomplete)
}

fn discover_parent_localisation_files_inner(
    parent_roots: &[PathBuf],
    primary_root: &Path,
    primary_files: &[DiscoveredPath],
    ignore_files: &[String],
    ignore_dirs: &[String],
    status: &mut ParentDiscoveryStatus,
) -> Vec<DiscoveredPath> {
    let primary: Vec<&str> = primary_files
        .iter()
        .map(|file| file.root_relative_path.as_str())
        .collect();
    visible_parent_files(
        parent_roots,
        primary_root,
        &primary,
        |root| {
            let mut config = FileManagerConfig {
                scan_budget: status.next_budget(),
                ..Default::default()
            };
            config.exclude_patterns.extend(ignore_files.iter().cloned());
            config
                .exclude_dir_patterns
                .extend(ignore_dirs.iter().cloned());
            match discover_workspace(WorkspaceDiscoveryRequest {
                roots: vec![root.to_path_buf()],
                kinds: vec![FileKind::Localisation],
                config,
                policy: DiscoveryPolicy::Workspace,
            }) {
                Ok(report) => {
                    if !status.record(report.examined, !report.failures.is_empty()) {
                        return Vec::new();
                    }
                    report
                        .files
                        .into_iter()
                        .filter(|file| file.kind == FileKind::Localisation)
                        .collect()
                }
                Err(error) => {
                    status.incomplete = true;
                    eprintln!(
                        "warn: parent mod localisation discovery failed for {}: {error}",
                        root.display()
                    );
                    Vec::new()
                }
            }
        },
        |file| file.root_relative_path.as_str(),
    )
}

/// The replace paths of the primary mod and any parents, applied to vanilla
/// files below them. Their order does not affect which base-game paths are
/// hidden, so this shadow is only used against the lower-priority game index.
pub fn vanilla_replacement_shadow(primary_root: &Path, parent_roots: &[PathBuf]) -> LayerShadow {
    let mut shadow = LayerShadow::default();
    shadow.claim_descriptor(primary_root);
    for root in parent_roots {
        shadow.claim_descriptor(root);
    }
    shadow
}

/// Remove indexed vanilla instances under a higher-priority mod layer's
/// `replace_path`s. `source` is the install-root path preserved by the vanilla
/// cache; matching is against its logical path and shares `LayerShadow`'s
/// separator, case and directory-boundary rules. A shadow without
/// `replace_path`s hides nothing, so the instances pass through untouched.
pub fn filter_vanilla_index(
    per_type: impl IntoIterator<Item = (String, Vec<(Arc<str>, cwtools_index::TypeInstance)>)>,
    vanilla_root: &Path,
    shadow: &LayerShadow,
) -> HashMap<String, Vec<(Arc<str>, cwtools_index::TypeInstance)>> {
    if !shadow.has_replace_paths() {
        return per_type.into_iter().collect();
    }
    // A source under the root as given needs no filesystem call. Only an
    // aliased one is resolved, once per distinct source, against a root
    // resolved once.
    let resolved_root =
        std::fs::canonicalize(vanilla_root).unwrap_or_else(|_| vanilla_root.to_path_buf());
    let mut visible_sources: HashMap<Arc<str>, bool> = HashMap::new();
    per_type
        .into_iter()
        .filter_map(|(type_name, instances)| {
            let visible: Vec<_> = instances
                .into_iter()
                .filter(|(source, _)| {
                    *visible_sources
                        .entry(Arc::clone(source))
                        .or_insert_with(|| {
                            let path = Path::new(source.as_ref());
                            let root = if path.starts_with(vanilla_root) {
                                vanilla_root
                            } else {
                                &resolved_root
                            };
                            !shadow.hides_file_under(root, path)
                        })
                })
                .collect();
            (!visible.is_empty()).then_some((type_name, visible))
        })
        .collect()
}

pub fn discover_and_parse_workspace(
    config: FileManagerConfig,
) -> Result<Vec<cwtools_file_manager::ParsedFile>, FileError> {
    let is_multi_mod = classify_directory(&config.root) == DirectoryType::MultipleMod;
    let mut manager = FileManager::new(config);
    if is_multi_mod {
        manager.discover_and_parse_multi_mod()
    } else {
        manager.discover_and_parse()
    }
}

pub fn discover_workspace(
    request: WorkspaceDiscoveryRequest,
) -> Result<DiscoveryReport, FileError> {
    let WorkspaceDiscoveryRequest {
        mut roots,
        kinds,
        mut config,
        policy,
    } = request;
    if roots.is_empty() {
        roots.push(config.root.clone());
    }
    if policy == DiscoveryPolicy::Vanilla {
        let defaults = FileManagerConfig::default();
        config.exclude_patterns = defaults.exclude_patterns;
        config.exclude_dir_patterns = defaults.exclude_dir_patterns;
    }

    let mut report = DiscoveryReport::default();
    let unlimited = config.scan_budget.max_files == 0;
    let mut remaining = config.scan_budget.files_remaining();
    if kinds.contains(&FileKind::Script) {
        for root in &roots {
            let mut script_config = config.clone();
            script_config.root = root.clone();
            // A one-file probe detects an omitted eligible file when the shared
            // countdown is already zero; zero in ScanBudget means unlimited.
            script_config.scan_budget.max_files = if unlimited { 0 } else { remaining.max(1) };
            let (files, failures, examined) =
                discover_workspace_files_with_failures(script_config)?;
            if !unlimited && remaining == 0 && examined > 0 {
                report.failures.push(
                    cwtools_file_manager::file_manager::DiscoveryFailure::file_count_budget(
                        root.clone(),
                    ),
                );
                return Ok(report);
            }
            remaining = remaining.saturating_sub(examined);
            report.examined = report.examined.saturating_add(examined);
            report.failures.extend(failures);
            report
                .files
                .extend(files.into_iter().map(|file| DiscoveredPath {
                    path: file.path,
                    root_relative_path: file.logical_path,
                    kind: FileKind::Script,
                }));
            if report
                .failures
                .iter()
                .any(|failure| failure.is_file_count_budget())
            {
                return Ok(report);
            }
        }
    }

    let other_kinds: Vec<FileKind> = kinds
        .into_iter()
        .filter(|kind| *kind != FileKind::Script)
        .collect();
    if !other_kinds.is_empty() {
        for root in &roots {
            let mut other_config = config.clone();
            other_config.root = root.clone();
            other_config.scan_budget.max_files = if unlimited { 0 } else { remaining.max(1) };
            let discovered = if classify_directory(root) == DirectoryType::MultipleMod {
                discover_paths_multi_mod(&other_config, &other_kinds)
            } else {
                discover_paths(&[root], &other_config, &other_kinds)?
            };
            if !unlimited && remaining == 0 && discovered.examined > 0 {
                report.failures.push(
                    cwtools_file_manager::file_manager::DiscoveryFailure::file_count_budget(
                        root.clone(),
                    ),
                );
                return Ok(report);
            }
            remaining = remaining.saturating_sub(discovered.examined);
            report.examined = report.examined.saturating_add(discovered.examined);
            report.files.extend(discovered.files);
            report.failures.extend(discovered.failures);
            if report
                .failures
                .iter()
                .any(|failure| failure.is_file_count_budget())
            {
                return Ok(report);
            }
        }
    }
    Ok(report)
}

pub fn discover_localisation_files(
    roots: &[PathBuf],
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: DiscoveryPolicy,
) -> Result<DiscoveryReport, FileError> {
    let mut config = FileManagerConfig::default();
    if policy == DiscoveryPolicy::Workspace {
        config.exclude_patterns.extend(ignore_files.iter().cloned());
        config
            .exclude_dir_patterns
            .extend(ignore_dirs.iter().cloned());
    }
    discover_workspace(WorkspaceDiscoveryRequest {
        roots: roots.to_vec(),
        kinds: vec![FileKind::Localisation],
        config,
        policy,
    })
}

pub fn search_config_for(directory: &Path) -> FileManagerConfig {
    let known_script_folders = [
        "common",
        "events",
        "history",
        "interface",
        "decisions",
        "missions",
        "gfx",
        "sound",
        "music",
        "static_modifiers",
        "buildings",
        "technologies",
        "ethics",
        "policies",
        "ship_sizes",
        "pop_faction",
        "starbases_consolidated",
        "traits",
        "edicts",
        "traditions",
        "ascension_perks",
        "governments",
        "country_types",
        "bypass",
        "dlc_list",
        "subject_types",
        "casus_belli",
        "war_goals",
        "bombardment_stances",
        "armies",
        "deposits",
        "planet_classes",
        "tile_blockers",
        "species_rights",
        "observation_station_missions",
        "star_classes",
        "ambient_objects",
        "name_lists",
        "notification_modifier",
        "component_tags",
        "event_chains",
        "personalities",
        "global_ship_designs",
        "graphical_cultures",
        "species_archetypes",
        "resources",
        "species_classes",
        "buildable_pops",
        "opinion_modifiers",
        "leader_class_enum",
        "asteroid_belt",
        "solar_system_initializers",
        "fallen_empires",
    ];
    let dir_name = directory.file_name().and_then(|n| n.to_str()).unwrap_or("");

    let script_exts = cwtools_file_manager::file_manager::SCRIPT_EXTENSIONS;
    let has_script_files = std::fs::read_dir(directory)
        .ok()
        .is_some_and(|mut entries| {
            entries.any(|e| {
                if let Ok(entry) = e {
                    entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| script_exts.contains(&ext))
                } else {
                    false
                }
            })
        });

    if known_script_folders.contains(&dir_name) || dir_name.ends_with(".txt") || has_script_files {
        FileManagerConfig {
            root: directory.to_path_buf(),
            include_dirs: vec![".".into()],
            ..Default::default()
        }
    } else {
        FileManagerConfig {
            root: directory.to_path_buf(),
            ..Default::default()
        }
    }
}

pub fn load_rules(
    rules: &RulesInput,
    table: &StringTable,
) -> Result<(RuleSet, Vec<RuleParseError>), String> {
    match rules {
        RulesInput::Dir(dir) => Ok(load_ruleset_from_dir(dir, table, ScanBudget::default())),
        RulesInput::File(file) => load_ruleset_from_file(file, table, ScanBudget::default()),
    }
}

#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_parse_keeps_the_fingerprint_it_was_parsed_from() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("source.txt");
        std::fs::write(&path, "a = 1\n").unwrap();
        let loaded = load_or_parse(
            &path,
            &StringTable::new(),
            None,
            ParseCacheUse::Full,
            None,
            None,
        )
        .unwrap()
        .expect("uncapped load");

        std::fs::write(&path, "a = 200\n").unwrap();
        assert_ne!(
            workspace_cache::source_cache_key(&path),
            loaded.fingerprint.metadata
        );
    }

    #[test]
    fn parse_discovered_files_for_index_treats_a_zero_cap_as_unlimited() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mod");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "x = 1\n").unwrap();
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let table = StringTable::new();
        let cache = open_parse_cache(&cache_dir, "hoi4", &root).expect("parse cache");
        let config = FileManagerConfig {
            root: root.clone(),
            max_file_size: 0,
            ..Default::default()
        };
        let parsed = parse_discovered_files_for_index(
            vec![DiscoveredFile {
                path: root.join("a.txt"),
                logical_path: "a.txt".to_string(),
            }],
            &table,
            Some(&cache),
            &config,
        )
        .unwrap();
        assert_eq!(parsed.len(), 1, "a zero cap must not refuse the file");
        assert_eq!(
            parsed[0].root_children.len(),
            1,
            "the file must be parsed whole"
        );
    }

    #[test]
    fn parse_discovered_files_for_index_rejects_the_whole_input_over_a_positive_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mod");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "x = 1\n").unwrap();
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let table = StringTable::new();
        let cache = open_parse_cache(&cache_dir, "hoi4", &root).expect("parse cache");
        let config = FileManagerConfig {
            root: root.clone(),
            max_file_size: 2,
            ..Default::default()
        };
        let parsed = parse_discovered_files_for_index(
            vec![DiscoveredFile {
                path: root.join("a.txt"),
                logical_path: "a.txt".to_string(),
            }],
            &table,
            Some(&cache),
            &config,
        );
        assert!(
            parsed.is_err(),
            "an over-cap file must be refused, not truncated"
        );
        assert!(
            !cache.wrote.load(Ordering::Relaxed),
            "a refused read must not write a cache entry"
        );
    }

    #[test]
    fn vanilla_auxiliary_discovery_rejects_partial_file_and_loc_walks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("vanilla");
        let budget = ScanBudget {
            max_files: 1,
            ..ScanBudget::default()
        };
        std::fs::create_dir_all(root.join("gfx")).unwrap();
        std::fs::write(root.join("gfx/a.dds"), "").unwrap();
        std::fs::write(root.join("gfx/b.dds"), "").unwrap();
        let index = TypeIndex::new();
        let cache = tmp.path().join("existing.cwv");
        std::fs::write(&cache, b"old cache").unwrap();
        let aux = build_vanilla_cache_aux_with_budget(&root, &index, None, budget);
        assert!(write_vanilla_cache_checked(&index, "hoi4", "fp", &cache, aux).is_err());
        assert_eq!(std::fs::read(&cache).unwrap(), b"old cache");
        std::fs::remove_file(&cache).unwrap();

        std::fs::remove_file(root.join("gfx/b.dds")).unwrap();
        std::fs::write(root.join("gfx/README.txt"), "ignored").unwrap();
        assert!(
            build_vanilla_cache_aux_with_budget(&root, &TypeIndex::new(), None, budget).is_ok()
        );
        std::fs::remove_file(root.join("gfx/a.dds")).unwrap();
        std::fs::create_dir_all(root.join("localisation")).unwrap();
        for name in ["a_l_english.yml", "b_l_english.yml"] {
            std::fs::write(
                root.join("localisation").join(name),
                "l_english:\n key:0 \"hi\"\n",
            )
            .unwrap();
        }
        assert!(
            build_vanilla_cache_aux_with_budget(&root, &TypeIndex::new(), None, budget).is_err()
        );
    }

    #[test]
    fn parent_discovery_rejects_partial_script_and_localisation_walks() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let parent = tmp.path().join("parent");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(parent.join("common")).unwrap();
        std::fs::create_dir_all(parent.join("localisation")).unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(parent.join("common").join(name), "x").unwrap();
        }
        for name in ["a_l_english.yml", "b_l_english.yml"] {
            std::fs::write(parent.join("localisation").join(name), "l_english:\n").unwrap();
        }
        let budget = ScanBudget {
            max_files: 1,
            ..ScanBudget::default()
        };
        let mut status = ParentDiscoveryStatus::new(budget);
        let scripts = discover_parent_script_files_inner(
            std::slice::from_ref(&parent),
            &primary,
            &[],
            None,
            &[],
            &[],
            &mut status,
        );
        assert!(parent_discovery_result(scripts, status.incomplete).is_err());
        let mut status = ParentDiscoveryStatus::new(budget);
        let locs = discover_parent_localisation_files_inner(
            std::slice::from_ref(&parent),
            &primary,
            &[],
            &[],
            &[],
            &mut status,
        );
        assert!(parent_discovery_result(locs, status.incomplete).is_err());

        std::fs::remove_file(parent.join("common/b.txt")).unwrap();
        std::fs::remove_file(parent.join("localisation/b_l_english.yml")).unwrap();
        let mut status = ParentDiscoveryStatus::new(budget);
        let scripts = discover_parent_script_files_inner(
            std::slice::from_ref(&parent),
            &primary,
            &[],
            None,
            &[],
            &[],
            &mut status,
        );
        assert_eq!(
            parent_discovery_result(scripts, status.incomplete)
                .unwrap()
                .len(),
            1
        );
        let mut status = ParentDiscoveryStatus::new(budget);
        let locs = discover_parent_localisation_files_inner(
            std::slice::from_ref(&parent),
            &primary,
            &[],
            &[],
            &[],
            &mut status,
        );
        assert_eq!(
            parent_discovery_result(locs, status.incomplete)
                .unwrap()
                .len(),
            1
        );

        let second = tmp.path().join("second_parent");
        std::fs::create_dir_all(second.join("common")).unwrap();
        std::fs::create_dir_all(second.join("localisation")).unwrap();
        std::fs::write(second.join("common/c.txt"), "x").unwrap();
        std::fs::write(second.join("localisation/c_l_english.yml"), "l_english:\n").unwrap();
        let parents = [parent, second];
        let mut status = ParentDiscoveryStatus::new(budget);
        let scripts = discover_parent_script_files_inner(
            &parents,
            &primary,
            &[],
            None,
            &[],
            &[],
            &mut status,
        );
        assert!(parent_discovery_result(scripts, status.incomplete).is_err());
        let mut status = ParentDiscoveryStatus::new(budget);
        let locs = discover_parent_localisation_files_inner(
            &parents,
            &primary,
            &[],
            &[],
            &[],
            &mut status,
        );
        assert!(parent_discovery_result(locs, status.incomplete).is_err());
    }

    #[test]
    fn workspace_discovery_shares_the_file_budget_across_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = [tmp.path().join("a"), tmp.path().join("b")];
        for root in &roots {
            std::fs::create_dir_all(root.join("common")).unwrap();
            std::fs::write(root.join("common/one.txt"), "x").unwrap();
        }
        let config = FileManagerConfig {
            root: roots[0].clone(),
            include_dirs: vec!["common".into()],
            scan_budget: ScanBudget {
                max_files: 1,
                ..ScanBudget::default()
            },
            ..Default::default()
        };
        let run = || {
            discover_workspace(WorkspaceDiscoveryRequest {
                roots: roots.to_vec(),
                kinds: vec![FileKind::Script],
                config: config.clone(),
                policy: DiscoveryPolicy::Workspace,
            })
            .unwrap()
        };
        let report = run();
        assert_eq!(report.files.len(), 1);
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.is_file_count_budget())
        );

        std::fs::remove_file(roots[1].join("common/one.txt")).unwrap();
        std::fs::write(roots[1].join("common/README.txt"), "ignored").unwrap();
        let report = run();
        assert_eq!(report.files.len(), 1);
        assert!(
            report.failures.is_empty(),
            "an excluded file after the exact cap is not exhaustion"
        );

        std::fs::create_dir_all(roots[1].join("gfx")).unwrap();
        std::fs::write(roots[1].join("gfx/icon.dds"), "").unwrap();
        let mixed = discover_workspace(WorkspaceDiscoveryRequest {
            roots: roots.to_vec(),
            kinds: vec![FileKind::Script, FileKind::Resource],
            config,
            policy: DiscoveryPolicy::Workspace,
        })
        .unwrap();
        assert_eq!(mixed.files.len(), 1);
        assert!(
            mixed
                .failures
                .iter()
                .any(|failure| failure.is_file_count_budget())
        );
    }

    #[test]
    fn cli_script_budget_applies_to_cold_and_cached_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mod");
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let paths = [root.join("a.txt"), root.join("b.txt")];
        for path in &paths {
            std::fs::write(path, "x = 1\n").unwrap();
        }
        let files: Vec<_> = paths
            .iter()
            .map(|path| DiscoveredFile {
                path: path.clone(),
                logical_path: path.file_name().unwrap().to_string_lossy().into_owned(),
            })
            .collect();
        let table = StringTable::new();
        let cache = open_parse_cache(&cache_dir, "hoi4", &root).unwrap();
        let run = |limit: u64, use_cache: bool| {
            let bytes = ScanBytes::new();
            let incomplete = AtomicBool::new(false);
            let budget = ParseReadBudget {
                max_file_size: u64::MAX,
                max_scan_bytes: limit,
                bytes: &bytes,
                incomplete: &incomplete,
            };
            let parsed =
                parse_discovered_files(files.clone(), &table, use_cache.then_some(&cache), &budget);
            (parsed.len(), incomplete.load(Ordering::Relaxed))
        };
        assert_eq!(
            run(6, false),
            (1, true),
            "cold source reads share one budget"
        );
        assert_eq!(run(12, false), (2, false), "an exact cap is complete");
        assert_eq!(run(0, true), (2, false), "prime both cache entries");
        cache.wrote.store(false, Ordering::Relaxed);
        assert_eq!(
            run(6, true),
            (1, true),
            "cache hits still consume the budget"
        );
        assert!(
            !cache.wrote.load(Ordering::Relaxed),
            "the accepted parse reused its cache"
        );
    }

    #[test]
    fn base_game_budget_rejects_partial_cold_and_cached_acquisition() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("vanilla");
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let files: Vec<_> = ["a.txt", "b.txt"]
            .into_iter()
            .map(|name| {
                let path = root.join(name);
                std::fs::write(&path, "x = 1\n").unwrap();
                DiscoveredFile {
                    path,
                    logical_path: name.into(),
                }
            })
            .collect();
        let table = StringTable::new();
        let cache = open_parse_cache(&cache_dir, "hoi4", &root).unwrap();
        let config = |max_bytes| FileManagerConfig {
            root: root.clone(),
            scan_budget: ScanBudget {
                max_bytes,
                ..ScanBudget::default()
            },
            ..Default::default()
        };
        assert!(parse_discovered_files_for_index(files.clone(), &table, None, &config(6)).is_err());
        assert_eq!(
            parse_discovered_files_for_index(files.clone(), &table, Some(&cache), &config(0))
                .unwrap()
                .len(),
            2
        );
        cache.wrote.store(false, Ordering::Relaxed);
        assert!(parse_discovered_files_for_index(files, &table, Some(&cache), &config(6)).is_err());
        assert!(
            !cache.wrote.load(Ordering::Relaxed),
            "the accepted source came from cache"
        );
    }

    #[test]
    fn validation_and_inline_rereads_reject_a_grown_source() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("script.txt");
        std::fs::write(&path, "x = 1\n").unwrap();
        let table = StringTable::new();
        let bytes = ScanBytes::new();
        let incomplete = AtomicBool::new(false);
        let budget = ParseReadBudget {
            max_file_size: 6,
            max_scan_bytes: 6,
            bytes: &bytes,
            incomplete: &incomplete,
        };
        let loaded = load_or_parse(
            &path,
            &table,
            None,
            ParseCacheUse::Full,
            Some(&budget),
            None,
        )
        .unwrap()
        .unwrap();
        let file = SourceFile {
            path: path.clone(),
            logical_path: "common/inline_scripts/test.txt".into(),
            fingerprint: loaded.fingerprint,
        };
        std::fs::write(&path, "x".repeat(64 * 1024)).unwrap();
        assert!(
            load_or_parse(
                &path,
                &table,
                None,
                ParseCacheUse::Full,
                None,
                Some(file.fingerprint.source_size)
            )
            .is_err()
        );
        let (_, inline_incomplete) = load_inline_scripts(&[file], &table);
        assert!(
            inline_incomplete,
            "a changed inline script makes the run incomplete"
        );
    }

    /// The filename must survive both halves of the fingerprint verbatim where
    /// it can, so a cache stays findable across runs (and shared with the LSP).
    #[test]
    fn vanilla_cache_path_is_sanitized_and_stable() {
        let path = vanilla_cache_path(Path::new("/cache"), "hoi4", "v1.19.2.0|rs:493876ece638460f");
        assert_eq!(
            path,
            Path::new("/cache/vanilla-hoi4-v1.19.2.0_rs_493876ece638460f.cwv")
        );
    }

    #[test]
    fn load_rules_returns_directory_read_errors_as_coded_diagnostics() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let table = StringTable::new();

        let (ruleset, errors) = load_rules(&RulesInput::Dir(file.path().to_path_buf()), &table)
            .expect("directory load remains non-fatal");

        assert!(ruleset.types.is_empty());
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert_eq!(errors[0].code, "CW600");
        assert_eq!(errors[0].severity, ErrorSeverity::Error);
        assert!(
            errors[0].to_string().starts_with(&format!(
                "{}:1:0: CW600: read directory error:",
                file.path().display()
            )),
            "error: {}",
            errors[0]
        );
    }

    #[test]
    fn vanilla_gated_checks_are_silent_once_the_index_is_loaded() {
        for game in [Game::Hoi4, Game::Stellaris, Game::Ck3] {
            assert!(vanilla_gated_checks(game, true).is_empty(), "{game}");
        }
    }

    #[test]
    fn vanilla_gated_checks_name_the_families_that_go_quiet() {
        assert_eq!(
            vanilla_gated_checks(Game::Hoi4, false),
            ["CW113", "CW222", "CW500"]
        );
        assert_eq!(
            vanilla_gated_checks(Game::Stellaris, false),
            ["CW113", "CW222", "CW227", "CW229", "CW250", "CW500"]
        );
    }

    #[test]
    fn default_cache_dir_is_absolute_and_honors_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Must be absolute on both Unix and Windows; a relative XDG/LAPPDATA
        // override would break the cache-key canonicalization.
        let dir = default_cache_dir().expect("cache dir");
        assert!(
            dir.is_absolute(),
            "{dir:?} must be absolute on {}",
            std::env::consts::OS
        );
        // XDG_CACHE_HOME wins over HOME on every platform.
        let tmp = tempfile::tempdir().unwrap();
        let xdg = tmp.path().join("xdg");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&xdg).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let prev_xdg = std::env::var("XDG_CACHE_HOME").ok();
        let prev_home = std::env::var("HOME").ok();
        let prev_la = std::env::var("LOCALAPPDATA").ok();
        struct EnvGuard {
            key: &'static str,
            prev: Option<String>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: test-only; serialized by ENV_LOCK
                unsafe {
                    if let Some(v) = self.prev.take() {
                        std::env::set_var(self.key, v);
                    } else {
                        std::env::remove_var(self.key);
                    }
                }
            }
        }
        let _g1 = EnvGuard {
            key: "XDG_CACHE_HOME",
            prev: prev_xdg,
        };
        let _g2 = EnvGuard {
            key: "HOME",
            prev: prev_home,
        };
        let _g3 = EnvGuard {
            key: "LOCALAPPDATA",
            prev: prev_la,
        };
        // SAFETY: test-only; serialized by ENV_LOCK
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", &xdg);
            std::env::set_var("HOME", &home);
            std::env::set_var("LOCALAPPDATA", "");
        }
        let resolved = default_cache_dir().unwrap();
        assert_eq!(resolved, xdg.join("cwtools"));
    }

    #[test]
    fn default_cache_dir_falls_back_to_temp_when_no_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_xdg = std::env::var("XDG_CACHE_HOME").ok();
        let prev_home = std::env::var("HOME").ok();
        let prev_la = std::env::var("LOCALAPPDATA").ok();
        struct EnvGuard {
            key: &'static str,
            prev: Option<String>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: test-only; serialized by ENV_LOCK
                unsafe {
                    if let Some(v) = self.prev.take() {
                        std::env::set_var(self.key, v);
                    } else {
                        std::env::remove_var(self.key);
                    }
                }
            }
        }
        let _g1 = EnvGuard {
            key: "XDG_CACHE_HOME",
            prev: prev_xdg,
        };
        let _g2 = EnvGuard {
            key: "HOME",
            prev: prev_home,
        };
        let _g3 = EnvGuard {
            key: "LOCALAPPDATA",
            prev: prev_la,
        };
        // SAFETY: test-only; serialized by ENV_LOCK
        unsafe {
            std::env::remove_var("XDG_CACHE_HOME");
            std::env::remove_var("HOME");
            std::env::remove_var("LOCALAPPDATA");
        }
        let dir = default_cache_dir().expect("fallback");
        assert!(dir.to_string_lossy().contains("cwtools"));
        assert!(dir.is_absolute());
    }

    #[test]
    fn load_loc_service_scopes_to_requested_languages() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        std::fs::write(loc.join("en_l_english.yml"), "l_english:\n k1:0 \"hi\"\n").unwrap();
        std::fs::write(
            loc.join("fr_l_french.yml"),
            "l_french:\n k2:0 \"bonjour\"\n",
        )
        .unwrap();
        let all = load_loc_service(&[tmp.path()], None, &[], &[]).unwrap();
        assert_eq!(all.files().len(), 2);
        let scoped = load_loc_service(&[tmp.path()], Some(&[Lang::English]), &[], &[]).unwrap();
        assert_eq!(scoped.files().len(), 1);
        assert_eq!(scoped.files()[0].lang, Some(Lang::English));
        std::fs::write(loc.join("names.csv"), "key;english;french\nfoo;Foo;Bar\n").unwrap();
        let scoped_with_csv =
            load_loc_service(&[tmp.path()], Some(&[Lang::English]), &[], &[]).unwrap();
        assert!(scoped_with_csv.files().iter().any(|f| f.is_csv));
    }

    #[test]
    fn load_loc_service_respects_ignore_globs() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        std::fs::write(loc.join("keep_l_english.yml"), "l_english:\n k:0 \"hi\"\n").unwrap();
        std::fs::write(
            loc.join("skip_me_l_english.yml"),
            "l_english:\n k2:0 \"hi\"\n",
        )
        .unwrap();
        let filtered = load_loc_service(&[tmp.path()], None, &["skip_*".to_string()], &[]).unwrap();
        assert_eq!(filtered.files().len(), 1);
        assert!(filtered.files()[0].path.contains("keep"));
    }

    #[test]
    fn localisation_discovery_layers_multi_mod_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (name, path, replace_path) in
            [("a", "mods/a", None), ("b", "mods/b", Some("localisation"))]
        {
            let descriptor = root.join("mod").join(format!("{name}.mod"));
            std::fs::create_dir_all(descriptor.parent().unwrap()).unwrap();
            let replace_path = replace_path
                .map(|path| format!("replace_path = \"{path}\"\n"))
                .unwrap_or_default();
            std::fs::write(
                descriptor,
                format!("name = \"{name}\"\npath = \"{path}\"\n{replace_path}"),
            )
            .unwrap();
        }
        for path in [
            "mods/a/localisation/shared_l_english.yml",
            "mods/a/localisation/nested/kept_l_english.yml",
            "mods/b/localisation/shared_l_english.yml",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }

        let discovery = discover_localisation_files(
            &[root.to_path_buf()],
            &[],
            &[],
            DiscoveryPolicy::Workspace,
        )
        .unwrap();

        assert_eq!(discovery.files.len(), 2);
        assert!(discovery.files.iter().any(|file| {
            file.path.starts_with(root.join("mods/b"))
                && file.root_relative_path == "localisation/shared_l_english.yml"
        }));
        assert!(discovery.files.iter().any(|file| {
            file.path.starts_with(root.join("mods/a"))
                && file.root_relative_path == "localisation/nested/kept_l_english.yml"
        }));
    }

    #[test]
    fn discovery_policy_keeps_workspace_ignores_out_of_vanilla() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("localisation/skip_l_english.yml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "l_english:\n k:0 \"hi\"\n").unwrap();
        let mut config = FileManagerConfig {
            root: tmp.path().to_path_buf(),
            ..Default::default()
        };
        config.exclude_patterns.push("skip_*".to_string());
        let roots = vec![tmp.path().to_path_buf()];

        let workspace = discover_workspace(WorkspaceDiscoveryRequest {
            roots: roots.clone(),
            kinds: vec![FileKind::Localisation],
            config: config.clone(),
            policy: DiscoveryPolicy::Workspace,
        })
        .unwrap();
        let vanilla = discover_workspace(WorkspaceDiscoveryRequest {
            roots,
            kinds: vec![FileKind::Localisation],
            config,
            policy: DiscoveryPolicy::Vanilla,
        })
        .unwrap();

        assert!(workspace.files.is_empty());
        assert_eq!(vanilla.files.len(), 1);
    }
}
