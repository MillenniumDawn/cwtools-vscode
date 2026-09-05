//! behind an `RwLock`, with no whole-workspace re-parse), which doesn't fit

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cwtools_cache::workspace::{self as workspace_cache, SourceCacheKey};
use cwtools_file_manager::file_manager::{
    DirectoryType, DiscoveredFile, DiscoveredPath, DiscoveryReport, FileError, FileKind,
    FileManager, FileManagerConfig, ScanBudget, ScanBytes, classify_directory, discover_paths,
    discover_paths_multi_mod,
};
use cwtools_file_manager::{read_text, read_text_capped};
use cwtools_game::constants::Game;
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_index::vanilla_cache::{self, VanillaCacheData};
use cwtools_index::{
    FileIndex, TypeIndex, collect_set_variable_names, collect_type_instances_with_subtypes,
    index_discovered_files, variable_defining_effects,
};
use cwtools_localization::{Lang, LocDiagnostic, LocIndex, LocService};
use cwtools_parser::ast::{ParseError, ParsedFile};
use cwtools_parser::parser::{parse_string, parse_string_without_comments};
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::RuleSet;
use cwtools_rules::ruleset_loader::{RuleParseError, load_ruleset_from_dir};
use cwtools_string_table::string_table::StringTable;
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
) -> Result<Option<LoadedParse>, FileError> {
    if let Some(cache) = cache {
        let cached = match cache_use {
            ParseCacheUse::Full => {
                workspace_cache::load_path(&cache.dir, cache.fingerprint, path, table)
            }
            ParseCacheUse::Index => {
                workspace_cache::load_path_for_index(&cache.dir, cache.fingerprint, path, table)
            }
        };
        if let Some((parsed, metadata)) = cached {
            return Ok(Some(LoadedParse {
                parsed,
                fingerprint: SourceFingerprint {
                    metadata: Some(metadata),
                    content_hash: None,
                },
            }));
        }
    }

    let metadata = workspace_cache::source_cache_key(path);
    let text = if let Some(read_budget) = read_budget {
        let (text, bytes) = read_text_capped(path, read_budget.max_file_size)?;
        if !read_budget
            .bytes
            .try_reserve(bytes, read_budget.max_scan_bytes)
        {
            eprintln!(
                "warn: skipping {}: scan byte budget exceeded",
                path.display()
            );
            return Ok(None);
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
            workspace_cache::store_path(
                &cache.dir,
                cache.fingerprint,
                path,
                source_key,
                &parsed,
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
        },
    }))
}

fn parse_discovered_files(
    files: Vec<DiscoveredFile>,
    table: &StringTable,
    cache: Option<&ParseCache>,
) -> Vec<ParsedFileSource> {
    use rayon::prelude::*;

    files
        .into_par_iter()
        .filter_map(|file| {
            match load_or_parse(&file.path, table, cache, ParseCacheUse::Full, None) {
                Ok(Some(loaded)) => Some(ParsedFileSource {
                    path: file.path,
                    logical_path: file.logical_path,
                    parsed: loaded.parsed,
                    fingerprint: loaded.fingerprint,
                }),
                Ok(None) => None,
                Err(error) => {
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
    cache: &ParseCache,
    config: &FileManagerConfig,
) -> Vec<cwtools_file_manager::file_manager::ParsedFile> {
    use rayon::prelude::*;

    let bytes = ScanBytes::new();
    let read_budget = ParseReadBudget {
        max_file_size: config.max_file_size,
        max_scan_bytes: config.scan_budget.max_bytes,
        bytes: &bytes,
    };
    files
        .into_par_iter()
        .filter_map(|file| {
            match load_or_parse(
                &file.path,
                table,
                Some(cache),
                ParseCacheUse::Index,
                Some(&read_budget),
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
                    eprintln!("warn: skipping {}: {}", file.path.display(), error);
                    None
                }
            }
        })
        .collect()
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

/// fingerprint matches the install short-circuits the walk, and a
#[derive(Debug, Clone)]
pub struct VanillaCacheAuto {
    /// Directory holding one cache file per game + fingerprint. Same layout and
    pub dir: PathBuf,
    pub refresh: bool,
}

pub struct SessionConfig<'a> {
    pub game: Game,
    pub rules: RulesInput,
    pub directory: PathBuf,
    pub vanilla: Option<PathBuf>,
    pub vanilla_cache: Option<VanillaCacheData>,
    pub vanilla_cache_auto: Option<VanillaCacheAuto>,
    pub ignore_files: &'a [String],
    pub ignore_dirs: &'a [String],
    pub loc_languages: Option<Vec<Lang>>,
    /// mod's own (live-walked) files. Off by default (Windows-authored mods);
    /// enable for mods that also target case-sensitive filesystems (Linux/Mac),
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
            vanilla,
            mut vanilla_cache,
            vanilla_cache_auto,
            ignore_files,
            ignore_dirs,
            loc_languages,
            case_sensitive_files,
            on_rules_diagnostic,
        } = config;

        let rules_table = StringTable::new();
        let (ruleset, rule_errors) = load_rules(&rules, &rules_table).unwrap_or_else(|e| {
            eprintln!("error: {}", e);
            (RuleSet::new(), Vec::new())
        });
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

        let discovery = discover_workspace(WorkspaceDiscoveryRequest {
            roots: vec![directory.clone()],
            kinds: vec![FileKind::Script, FileKind::Localisation],
            config: fm_config,
            policy: DiscoveryPolicy::Workspace,
        });
        let (files, loc_paths, discovery_failed) = match discovery {
            Ok(discovery) => {
                for failure in discovery.failures {
                    eprintln!(
                        "warn: discovery skipped {}: {}",
                        failure.path.display(),
                        failure.error
                    );
                }
                let files = discovery
                    .files
                    .iter()
                    .filter(|file| file.kind == FileKind::Script)
                    .map(|file| DiscoveredFile {
                        path: file.path.clone(),
                        logical_path: file.root_relative_path.clone(),
                    })
                    .collect();
                let loc_paths = discovery
                    .files
                    .into_iter()
                    .filter(|file| file.kind == FileKind::Localisation)
                    .map(|file| file.path)
                    .collect();
                (
                    parse_discovered_files(files, &rules_table, parse_cache.as_ref()),
                    loc_paths,
                    false,
                )
            }
            Err(e) => {
                eprintln!("error: discovery failed for {}: {}", directory.display(), e);
                (Vec::new(), Vec::new(), true)
            }
        };
        if let Some(cache) = &parse_cache
            && cache.wrote.swap(false, Ordering::Relaxed)
        {
            workspace_cache::prune(&cache.dir, cache.fingerprint);
        }

        let parsed = files;

        use rayon::prelude::*;
        type PerFileResult = (
            HashMap<String, Vec<cwtools_index::TypeInstance>>,
            HashMap<String, Vec<cwtools_index::TypeInstance>>,
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
                let mut var_names: Vec<String> = Vec::new();
                collect_set_variable_names(&src.parsed, &rules_table, &var_effects, &mut var_names);
                let value_sets = cwtools_index::dynamic_values::collect_value_set_members(
                    &ruleset,
                    &src.parsed,
                    &rules_table,
                );
                let complex = cwtools_index::dynamic_values::collect_complex_enum_values(
                    &ruleset,
                    &src.parsed,
                    &src.logical_path,
                    &rules_table,
                );
                // reported as an unknown command (#348). Path-driven: the HOI4
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
            src,
            (
                instances,
                subtype_instances,
                var_names,
                value_sets,
                complex,
                scripted_locs,
                scripted_guis,
            ),
        ) in parsed.iter().zip(per_file)
        {
            let file_uri = src.path.to_str().unwrap_or("");
            type_index.merge(file_uri, instances);
            if !subtype_instances.is_empty() {
                type_index.merge(file_uri, subtype_instances);
            }
            for n in &var_names {
                type_index.var_index.add_name(n);
            }
            type_index.value_set_values.merge_file(file_uri, value_sets);
            type_index.complex_enum_values.merge_file(file_uri, complex);
            type_index
                .scripted_loc_index
                .merge_file(file_uri, scripted_locs);
            type_index
                .scripted_gui_index
                .merge_file(file_uri, scripted_guis);
        }
        let source_files: Vec<SourceFile> = parsed
            .into_iter()
            .map(|src| SourceFile {
                path: src.path,
                logical_path: src.logical_path,
                fingerprint: src.fingerprint,
            })
            .collect();

        let inline_scripts = load_inline_scripts(&source_files, &rules_table);

        let mut cache_write_target: Option<(PathBuf, String)> = None;
        let mut force_vanilla_rebuild = false;
        if let (None, Some(auto), Some(vanilla_dir)) =
            (&vanilla_cache, &vanilla_cache_auto, &vanilla)
        {
            let fingerprint = vanilla_cache::combined_fingerprint(vanilla_dir, &ruleset);
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
            type_index.merge_base_game_with_uris(cache.per_type);
            let aux = cache.aux;
            for n in &aux.var_names {
                type_index.var_index.add_name(n);
            }
            type_index.file_index = build_file_index(
                &directory,
                ignore_files,
                ignore_dirs,
                VanillaFiles::Cached(aux.file_paths),
                case_sensitive_files,
            );
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
                .set_vanilla_names(aux.scripted_loc_names);
            type_index
                .scripted_gui_index
                .set_vanilla_names(aux.scripted_gui_names);
            cached_loc_keys = Some(aux.loc_keys);
        } else if let Some(vanilla_dir) = &vanilla {
            let vanilla_index = if let Some(missing_vanilla) = &missing_vanilla {
                Err(FileError::MissingRoot(missing_vanilla.clone()))
            } else if let Some(parse_cache_dir) = &vanilla_parse_cache_dir
                && !force_vanilla_rebuild
            {
                index_game_dir_with_parse_cache(
                    vanilla_dir,
                    &ruleset,
                    &rules_table,
                    &var_effects,
                    parse_cache_dir,
                    &game_id,
                )
            } else {
                index_game_dir(vanilla_dir, &ruleset, &rules_table, &var_effects)
            };
            match vanilla_index {
                Ok(vanilla_index) => {
                    has_vanilla_data = true;
                    if let Some((path, fingerprint)) = &cache_write_target {
                        let aux = build_vanilla_cache_aux(vanilla_dir, &vanilla_index);
                        match write_vanilla_cache(&vanilla_index, &game_id, fingerprint, path, aux)
                        {
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
                    type_index.scripted_loc_index.set_vanilla_names(
                        vanilla_index
                            .scripted_loc_index
                            .names()
                            .map(str::to_string)
                            .collect(),
                    );
                    type_index.scripted_gui_index.set_vanilla_names(
                        vanilla_index
                            .scripted_gui_index
                            .names()
                            .map(str::to_string)
                            .collect(),
                    );
                    for (type_name, entries) in vanilla_index.map {
                        let per_type = HashMap::from([(
                            type_name,
                            entries.into_iter().map(|(_, inst)| inst).collect(),
                        )]);
                        type_index.merge_base_game("<vanilla>", per_type);
                    }
                    type_index.file_index = build_file_index(
                        &directory,
                        ignore_files,
                        ignore_dirs,
                        VanillaFiles::Install(vanilla_dir),
                        case_sensitive_files,
                    );
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

        if has_vanilla_data && !vanilla_discovery_failed {
            type_index.complete = true;
        }

        let modifier_keys = build_modifier_keys(&ruleset, &type_index);

        let mut loc_service =
            LocService::from_paths(loc_paths, ScanBudget::default(), loc_languages.as_deref());
        let mut loc_index = LocIndex::build_scoped(&loc_service, loc_languages.as_deref());
        if cached_loc_keys.is_none()
            && let Some(v) = &vanilla
        {
            let vanilla_paths =
                localisation_paths(std::slice::from_ref(v), &[], &[], DiscoveryPolicy::Vanilla);
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
        .with_source_files(source_files, discovery_failed || vanilla_discovery_failed)
    }

    fn with_source_files(self, files: Vec<SourceFile>, discovery_failed: bool) -> SessionWithFiles {
        SessionWithFiles {
            session: self,
            files,
            discovery_failed,
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
}

impl std::ops::Deref for SessionWithFiles {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}

impl SessionWithFiles {
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
                let file_str: cwtools_validation::FilePath =
                    std::sync::Arc::from(src.path.to_str().unwrap_or(""));
                let changed_error = || {
                    (
                        src.path.clone(),
                        vec![ValidationError {
                            message: format!(
                                "file changed after indexing: {file_str}; rerun validation"
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
                ) {
                    Ok(Some(loaded)) if loaded.fingerprint == src.fingerprint => loaded.parsed,
                    Ok(_) => return changed_error(),
                    Err(error) => {
                        return (
                            src.path.clone(),
                            vec![ValidationError {
                                message: format!("could not parse {file_str}: {error}"),
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
                // CW100: objects defined here whose `## required` localisation
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
                    std::sync::Arc::from(path.to_str().unwrap_or(""));
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

fn load_inline_scripts(files: &[SourceFile], table: &StringTable) -> InlineScripts {
    let mut scripts = InlineScripts::default();
    for file in files
        .iter()
        .filter(|file| InlineScripts::is_script_path(&file.logical_path))
    {
        match read_text(&file.path) {
            Ok(text) => {
                scripts.insert(
                    &file.logical_path,
                    parse_string_without_comments(&text, table),
                );
            }
            Err(error) => eprintln!(
                "warn: skipping inline script {}: {}",
                file.path.display(),
                error
            ),
        }
    }
    scripts
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

fn localisation_paths(
    roots: &[PathBuf],
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: DiscoveryPolicy,
) -> Vec<PathBuf> {
    match discover_localisation_files(roots, ignore_files, ignore_dirs, policy) {
        Ok(discovery) => {
            for failure in discovery.failures {
                eprintln!(
                    "warn: discovery skipped {}: {}",
                    failure.path.display(),
                    failure.error
                );
            }
            discovery
                .files
                .into_iter()
                .filter(|file| file.kind == FileKind::Localisation)
                .map(|file| file.path)
                .collect()
        }
        Err(error) => {
            eprintln!("error: localisation discovery failed: {error}");
            Vec::new()
        }
    }
}

pub fn load_loc_service(
    dirs: &[&Path],
    langs: Option<&[Lang]>,
    ignore_files: &[String],
    ignore_dirs: &[String],
) -> LocService {
    let roots: Vec<PathBuf> = dirs.iter().map(|dir| (*dir).to_path_buf()).collect();
    let paths = localisation_paths(
        &roots,
        ignore_files,
        ignore_dirs,
        DiscoveryPolicy::Workspace,
    );
    LocService::from_paths(paths, ScanBudget::default(), langs)
}

/// Cache file for `game` at `fingerprint` under `dir`. The name comes from the
fn vanilla_cache_path(dir: &Path, game: &str, fingerprint: &str) -> PathBuf {
    dir.join(vanilla_cache::cache_file_name(game, fingerprint))
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

/// by an older format, stale fingerprint) reads as a miss so the caller
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
    Cached(Vec<String>),
    Install(&'a Path),
}

/// so the editor and the CLI answer the same reference the same way (#283).
pub fn build_file_index(
    workspace_root: &Path,
    workspace_ignore_files: &[String],
    workspace_ignore_dirs: &[String],
    vanilla: VanillaFiles<'_>,
    case_sensitive: bool,
) -> FileIndex {
    let mut index = FileIndex::new();
    index.set_case_sensitive(case_sensitive);
    index.add_paths(discover_file_index_paths(
        workspace_root,
        workspace_ignore_files,
        workspace_ignore_dirs,
        DiscoveryPolicy::Workspace,
    ));
    match vanilla {
        VanillaFiles::Cached(paths) => index.add_paths(paths),
        VanillaFiles::Install(dir) => index.add_paths(discover_file_index_paths(
            dir,
            &[],
            &[],
            DiscoveryPolicy::Vanilla,
        )),
    }
    index
}

fn discover_file_index_paths(
    root: &Path,
    ignore_files: &[String],
    ignore_dirs: &[String],
    policy: DiscoveryPolicy,
) -> Vec<String> {
    let mut config = FileManagerConfig {
        include_dirs: vec![".".to_string()],
        ..Default::default()
    };
    if policy == DiscoveryPolicy::Workspace {
        config.exclude_patterns.extend(ignore_files.iter().cloned());
        config
            .exclude_dir_patterns
            .extend(ignore_dirs.iter().cloned());
    }
    match discover_workspace(WorkspaceDiscoveryRequest {
        roots: vec![root.to_path_buf()],
        kinds: vec![FileKind::Script, FileKind::Localisation, FileKind::Resource],
        config,
        policy,
    }) {
        Ok(discovery) => {
            for failure in discovery.failures {
                eprintln!(
                    "warn: discovery skipped {}: {}",
                    failure.path.display(),
                    failure.error
                );
            }
            discovery
                .files
                .into_iter()
                .map(|file| file.root_relative_path)
                .collect()
        }
        Err(error) => {
            eprintln!(
                "error: file discovery failed for {}: {error}",
                root.display()
            );
            Vec::new()
        }
    }
}

pub fn build_vanilla_cache_aux(
    vanilla_dir: &Path,
    index: &TypeIndex,
) -> cwtools_index::vanilla_cache::VanillaCacheAux {
    let loc_paths = localisation_paths(
        &[vanilla_dir.to_path_buf()],
        &[],
        &[],
        DiscoveryPolicy::Vanilla,
    );
    let loc_service = LocService::from_paths(loc_paths, ScanBudget::default(), None);
    let loc_keys = cwtools_localization::loc_index::per_language_keys(&loc_service);
    let mut file_index = cwtools_index::FileIndex::new();
    // Collect on-disk case so a later case-sensitive run can case-check vanilla
    file_index.set_case_sensitive(true);
    file_index.add_paths(discover_file_index_paths(
        vanilla_dir,
        &[],
        &[],
        DiscoveryPolicy::Vanilla,
    ));
    cwtools_index::vanilla_cache::VanillaCacheAux {
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
    }
}

pub fn index_game_dir(
    dir: &Path,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: &HashSet<String>,
) -> Result<TypeIndex, FileError> {
    index_game_dir_with_cache(dir, ruleset, table, var_effects, None)
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
    index_game_dir_with_cache(dir, ruleset, table, var_effects, cache.as_ref())
}

fn index_game_dir_with_cache(
    dir: &Path,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: &HashSet<String>,
    cache: Option<&ParseCache>,
) -> Result<TypeIndex, FileError> {
    let mut config = search_config_for(dir);
    apply_config_folders(&mut config, &ruleset.folders);
    let mut mgr = FileManager::with_string_table(config, table.clone());
    let files = if let Some(cache) = cache {
        let files = mgr.discover_files()?;
        parse_discovered_files_for_index(files, table, cache, &mgr.config)
    } else {
        mgr.discover_and_parse()?
    };
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

/// discover the same file set (#284).
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

/// workspaces, so the CLI and LSP share one branching point (#284).
pub fn discover_workspace_files(
    config: FileManagerConfig,
) -> Result<Vec<DiscoveredFile>, FileError> {
    let is_multi_mod = classify_directory(&config.root) == DirectoryType::MultipleMod;
    let manager = FileManager::new(config);
    if is_multi_mod {
        Ok(manager.discover_files_multi_mod())
    } else {
        manager.discover_files()
    }
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
    if kinds.contains(&FileKind::Script) {
        for root in &roots {
            let mut script_config = config.clone();
            script_config.root = root.clone();
            let files = discover_workspace_files(script_config)?;
            report
                .files
                .extend(files.into_iter().map(|file| DiscoveredPath {
                    path: file.path,
                    root_relative_path: file.logical_path,
                    kind: FileKind::Script,
                }));
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
            let discovered = if classify_directory(root) == DirectoryType::MultipleMod {
                discover_paths_multi_mod(&other_config, &other_kinds)
            } else {
                discover_paths(&[root], &other_config, &other_kinds)?
            };
            report.files.extend(discovered.files);
            report.failures.extend(discovered.failures);
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
        RulesInput::File(file) => {
            let rules_str = std::fs::read_to_string(file)
                .map_err(|e| format!("could not read rules {}: {}", file.display(), e))?;
            Ok((
                ast_to_ruleset(&parse_string(&rules_str, table), table),
                Vec::new(),
            ))
        }
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
        let loaded = load_or_parse(&path, &StringTable::new(), None, ParseCacheUse::Full, None)
            .unwrap()
            .expect("uncapped load");

        std::fs::write(&path, "a = 200\n").unwrap();
        assert_ne!(
            workspace_cache::source_cache_key(&path),
            loaded.fingerprint.metadata
        );
    }

    /// The filename must survive both halves of the fingerprint verbatim where
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
        let all = load_loc_service(&[tmp.path()], None, &[], &[]);
        assert_eq!(all.files().len(), 2);
        let scoped = load_loc_service(&[tmp.path()], Some(&[Lang::English]), &[], &[]);
        assert_eq!(scoped.files().len(), 1);
        assert_eq!(scoped.files()[0].lang, Some(Lang::English));
        std::fs::write(loc.join("names.csv"), "key;english;french\nfoo;Foo;Bar\n").unwrap();
        let scoped_with_csv = load_loc_service(&[tmp.path()], Some(&[Lang::English]), &[], &[]);
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
        let filtered = load_loc_service(&[tmp.path()], None, &["skip_*".to_string()], &[]);
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

        assert_eq!(discovery.files.len(), 1);
        assert!(discovery.files[0].path.starts_with(root.join("mods/b")));
        assert_eq!(
            discovery.files[0].root_relative_path,
            "localisation/shared_l_english.yml"
        );
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
