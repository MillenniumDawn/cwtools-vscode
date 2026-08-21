use crate::post_process::post_process;
#[cfg(test)]
use crate::rules_converter::ast_to_ruleset;
use crate::rules_converter::{ast_to_ruleset_raw, validate_comment_directives};
use crate::rules_types::{CwtDefKind, CwtDefPosition, RuleSet};
use cwtools_error_codes::{
    CW600_RULES_FILE_UNREADABLE, CW602_RULES_UNEXPANDED_ALIAS, ErrorCode, ErrorSeverity,
};
use cwtools_file_manager::file_manager::{ScanBudget, ScanBytes, read_text_capped};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::path::Path;

/// A non-fatal error from loading a `.cwt` rules directory: a file that failed
/// to read, or whose rules didn't hold up. Carries the source location so the
/// LSP can publish a diagnostic on the offending file and reveal where the
/// rules broke, and the catalog code/severity so the CLI can report it as an
/// ordinary diagnostic and fail CI on it.
#[derive(Debug, Clone)]
pub struct RuleParseError {
    pub file: std::path::PathBuf,
    /// 1-based line. `1` for read errors and anything else without a position.
    pub line: u32,
    pub col: u16,
    /// The catalog id this reports under (`CW600`-`CW603`).
    pub code: &'static str,
    pub severity: ErrorSeverity,
    pub message: String,
}

impl RuleParseError {
    /// A rules-config problem at `file:line:col`, reported under the `code`
    /// catalog entry. Use this rather than the fields so a new emit site can't
    /// invent a code/severity pairing the catalog doesn't have.
    pub fn new(
        code: &ErrorCode,
        file: std::path::PathBuf,
        line: u32,
        col: u16,
        message: String,
    ) -> Self {
        Self {
            file,
            line,
            col,
            code: code.id,
            severity: code.severity,
            message,
        }
    }
}

impl std::fmt::Display for RuleParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}: {}",
            self.file.display(),
            self.line,
            self.col,
            self.code,
            self.message
        )
    }
}

fn directory_read_error(dir: &Path, error: std::io::Error) -> RuleParseError {
    RuleParseError::new(
        &CW600_RULES_FILE_UNREADABLE,
        dir.to_path_buf(),
        1,
        0,
        format!("read directory error: {error}"),
    )
}

/// Place an unexpanded `single_alias` reference on its own definition, so the
/// diagnostic lands where the fix goes. Falls back to the rules directory when
/// the definition has no recorded position (nothing defines it, or the ruleset
/// was built by hand).
fn alias_expansion_error(
    dir: &Path,
    ruleset: &RuleSet,
    error: crate::post_process::AliasExpansionError,
) -> RuleParseError {
    let position = ruleset
        .def_positions
        .iter()
        .find(|p| p.kind == CwtDefKind::SingleAlias && p.name == error.name);
    RuleParseError::new(
        &CW602_RULES_UNEXPANDED_ALIAS,
        position.map_or_else(|| dir.to_path_buf(), |p| p.file.clone()),
        position.map_or(1, |p| p.line),
        position.map_or(0, |p| p.col),
        error.message,
    )
}

/// Recursively collect all `*.cwt` files under `dir`. Symlinks and non-regular
/// files are rejected outright (see `file_manager::walk_dir_generic`), and the
/// walk stops once `remaining_files` reaches 0.
fn collect_cwt_files(
    dir: &Path,
    out: &mut Vec<std::path::PathBuf>,
    errors: &mut Vec<RuleParseError>,
    remaining_files: &mut usize,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(directory_read_error(dir, error));
            return;
        }
    };

    let mut kids: Vec<(std::ffi::OsString, std::path::PathBuf, std::fs::FileType)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(directory_read_error(dir, error));
                continue;
            }
        };
        let Ok(ft) = entry.file_type() else { continue };
        kids.push((entry.file_name(), entry.path(), ft));
    }
    kids.sort_by(|a, b| a.0.cmp(&b.0));

    for (_name, path, ft) in kids {
        if *remaining_files == 0 {
            break;
        }
        if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
            continue;
        }
        if ft.is_dir() {
            collect_cwt_files(&path, out, errors, remaining_files);
        } else if path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("cwt"))
            .unwrap_or(false)
        {
            *remaining_files -= 1;
            out.push(path);
        }
    }
}

/// Merge `src` into `dst`, extending all collections.
pub fn merge_ruleset(dst: &mut RuleSet, src: RuleSet) {
    dst.types.extend(src.types);
    dst.enums.extend(src.enums);
    dst.aliases.extend(src.aliases);
    dst.single_aliases.extend(src.single_aliases);
    dst.complex_enums.extend(src.complex_enums);
    dst.root_rules.extend(src.root_rules);
    for (name, vals) in src.values {
        dst.values.entry(name).or_default().extend(vals);
    }
    dst.modifiers.extend(src.modifiers);
    dst.modifier_categories.extend(src.modifier_categories);
    dst.scope_links.extend(src.scope_links);
    dst.scope_inputs.extend(src.scope_inputs);
    dst.link_inputs.extend(src.link_inputs);
    dst.folders.extend(src.folders);
    dst.localisation_commands.extend(src.localisation_commands);
}

/// Parse a `folders.cwt`: one folder name per line, `#` comments and blank
/// lines skipped. Not Paradox-script syntax, so it bypasses the rules
/// converter entirely.
fn parse_folders_list(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

/// What one `.cwt` file contributes to the merged ruleset. Each is built
/// independently so the read/parse/convert fan-out can run in parallel; the
/// merge folds them back in walk order.
struct FileRules {
    ruleset: RuleSet,
    ref_candidates: Vec<crate::config_validation::RefCandidate>,
    def_positions: Vec<CwtDefPosition>,
    errors: Vec<RuleParseError>,
}

/// Read, parse and convert one `.cwt` file. Touches no shared state but the
/// string table and the scan's byte counter, both of which are built for it.
fn load_cwt_file(
    path: &Path,
    table: &StringTable,
    budget: ScanBudget,
    bytes: &ScanBytes,
) -> FileRules {
    let mut out = FileRules {
        ruleset: RuleSet::new(),
        ref_candidates: Vec::new(),
        def_positions: Vec::new(),
        errors: Vec::new(),
    };
    let unreadable = |message: String| {
        RuleParseError::new(
            &CW600_RULES_FILE_UNREADABLE,
            path.to_path_buf(),
            1,
            0,
            message,
        )
    };
    match read_text_capped(path, budget.max_file_size) {
        Ok((content, n)) => {
            if !bytes.try_reserve(n, budget.max_bytes) {
                out.errors
                    .push(unreadable("scan byte budget exceeded".to_string()));
                return out;
            }
            if path
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("folders.cwt"))
            {
                out.ruleset.folders = parse_folders_list(&content);
            } else {
                let parsed = parse_string(&content, table);
                out.errors
                    .extend(validate_comment_directives(&parsed, path));
                out.ruleset = ast_to_ruleset_raw(&parsed, table);
                crate::config_validation::collect_reference_candidates(
                    path,
                    &parsed,
                    table,
                    &mut out.ref_candidates,
                );
                crate::config_validation::collect_definition_positions(
                    path,
                    &parsed,
                    table,
                    &mut out.def_positions,
                );
            }
        }
        Err(e) => out.errors.push(unreadable(format!("read error: {}", e))),
    }
    out
}

/// Walk `dir` for `*.cwt` files, parse each with `table`, convert via
/// `ast_to_ruleset`, and merge all results into one `RuleSet`.
///
/// Files are read and converted in parallel and merged in sorted walk order, so
/// the result is what loading them one at a time produces. The one thing that
/// does move is which files a spent `max_bytes` refuses, since the reservations
/// no longer land in walk order.
///
/// Returns `(ruleset, errors)`. Errors are non-fatal: a file that fails to read
/// is skipped and its message collected.
pub fn load_ruleset_from_dir(
    dir: &Path,
    table: &StringTable,
    budget: ScanBudget,
) -> (RuleSet, Vec<RuleParseError>) {
    use rayon::prelude::*;

    let mut cwt_files = Vec::new();
    let mut errors = Vec::new();
    let mut remaining = budget.max_files;
    collect_cwt_files(dir, &mut cwt_files, &mut errors, &mut remaining);

    let bytes = ScanBytes::new();
    let per_file: Vec<FileRules> = cwt_files
        .par_iter()
        .map(|path| load_cwt_file(path, table, budget, &bytes))
        .collect();

    let mut combined = RuleSet::new();
    // Lightweight reference candidates collected from each AST while it is alive.
    // The AST itself is dropped as soon as it is converted; only these positioned
    // (kind, name) records are retained for the post-merge resolution pass, so we
    // never pin more parsed `.cwt` files than there are threads.
    let mut ref_candidates: Vec<crate::config_validation::RefCandidate> = Vec::new();
    for file in per_file {
        errors.extend(file.errors);
        merge_ruleset(&mut combined, file.ruleset);
        ref_candidates.extend(file.ref_candidates);
        combined.def_positions.extend(file.def_positions);
    }

    // Run the post-processing pipeline once all files have been merged so that
    // cross-file single_alias references are fully resolved. Anything expansion
    // refused (a cycle, a chain past the depth limit, the node budget) comes
    // back as a diagnostic on the definition it names.
    let refused = post_process(&mut combined);
    errors.extend(
        refused
            .into_iter()
            .map(|error| alias_expansion_error(dir, &combined, error)),
    );

    // Build alias lookup indexes last — alias names/order are stable after this.
    combined.reindex();

    // Structural validation: now that every definition is merged and indexed,
    // resolve the references collected during conversion against the merged set,
    // flagging any pointing at an undefined type/enum/single_alias.
    errors.extend(crate::config_validation::resolve_reference_candidates(
        &ref_candidates,
        &combined,
    ));

    (combined, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// merge_ruleset must carry scope_links across files. links.cwt is a separate
    /// file from the type/alias files, so dropping scope_links here silently breaks
    /// from-data scope-link recognition (e.g. `character = { ... }`) for the whole
    /// merged ruleset.
    #[test]
    fn merge_preserves_scope_links() {
        let table = StringTable::new();
        let links = parse_string("links = { character = { from_data = yes } }", &table);
        let mut a = ast_to_ruleset(&links, &table);

        let other = parse_string("types = { type[evt] = { path = \"game/events\" } }", &table);
        let b = ast_to_ruleset(&other, &table);

        merge_ruleset(&mut a, b);
        assert!(
            a.scope_links.contains("character"),
            "scope_links lost during merge"
        );
    }

    /// The rules walk must reject symlinks: a symlink can point outside the
    /// rules dir or into a cycle.
    #[cfg(unix)]
    #[test]
    fn collect_cwt_files_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("real.cwt"), "types = { }\n").unwrap();
        symlink(tmp.path().join("real.cwt"), tmp.path().join("link.cwt")).unwrap();

        let mut files = Vec::new();
        let mut errors = Vec::new();
        let mut remaining = ScanBudget::default().max_files;
        collect_cwt_files(tmp.path(), &mut files, &mut errors, &mut remaining);
        assert!(errors.is_empty());
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"real.cwt".to_string()));
        assert!(
            !names.contains(&"link.cwt".to_string()),
            "symlinked .cwt file must be rejected: {names:?}"
        );
    }

    /// A `single_alias` expansion the post-processor refuses has to reach the
    /// caller like any other rules problem, on the definition it names rather
    /// than on the directory, so the editor can point at the line to fix.
    #[test]
    fn load_ruleset_from_dir_reports_unexpanded_single_alias() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("cycle.cwt"),
            "single_alias[loop_a] = {\n    x = single_alias_right[loop_b]\n}\n\
             single_alias[loop_b] = {\n    y = single_alias_right[loop_a]\n}\n",
        )
        .unwrap();

        let table = StringTable::new();
        let (_, errors) = load_ruleset_from_dir(tmp.path(), &table, ScanBudget::default());

        let cycle = errors
            .iter()
            .find(|e| e.message.contains("reference cycle"))
            .unwrap_or_else(|| panic!("no cycle diagnostic in {errors:?}"));
        assert!(cycle.file.ends_with("cycle.cwt"), "error: {cycle}");
        assert_eq!(cycle.line, 1, "error: {cycle}");
        assert_eq!(
            (cycle.code, cycle.severity),
            ("CW602", ErrorSeverity::Error)
        );
    }

    /// A `.cwt` file over the per-file cap must be skipped, not read to EOF.
    #[test]
    fn load_ruleset_from_dir_skips_over_limit_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("ok.cwt"), "types = { }\n").unwrap();
        std::fs::write(tmp.path().join("huge.cwt"), "x".repeat(200)).unwrap();

        let table = StringTable::new();
        let budget = ScanBudget {
            max_file_size: 50,
            ..ScanBudget::default()
        };
        let (ruleset, errors) = load_ruleset_from_dir(tmp.path(), &table, budget);
        assert!(
            !errors.iter().any(|e| e.file.ends_with("ok.cwt")),
            "ok.cwt must parse clean: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.file.ends_with("huge.cwt")),
            "over-cap file must be reported as a read error: {errors:?}"
        );
        let _ = ruleset;
    }

    #[test]
    fn localisation_commands_are_parsed_and_merged() {
        let table = StringTable::new();
        let a = parse_string(
            "localisation_commands = { GetName = any GetTag = { any } <scripted_loc> = any }",
            &table,
        );
        let mut ra = ast_to_ruleset(&a, &table);
        assert!(ra.localisation_commands.contains("getname"));
        assert!(ra.localisation_commands.contains("gettag"));
        assert!(
            !ra.localisation_commands.contains("<scripted_loc>"),
            "placeholder must be skipped"
        );
        assert_eq!(ra.localisation_commands.len(), 2);

        let b = parse_string("localisation_commands = { GetLeader = any }", &table);
        let rb = ast_to_ruleset(&b, &table);
        merge_ruleset(&mut ra, rb);
        assert!(ra.localisation_commands.contains("getleader"));
        assert_eq!(ra.localisation_commands.len(), 3);
    }

    #[test]
    fn localisation_commands_case_folding_and_quoting() {
        let table = StringTable::new();
        let cwt = r#"localisation_commands = { "GetName" = any getname = any GETNAME = any }"#;
        let parsed = parse_string(cwt, &table);
        let rs = ast_to_ruleset(&parsed, &table);
        assert_eq!(
            rs.localisation_commands.len(),
            1,
            "duplicate case variants must dedupe lowercased: {:?}",
            rs.localisation_commands
        );
        assert!(rs.localisation_commands.contains("getname"));
    }

    #[test]
    fn localisation_commands_placeholder_quoting_and_malformed() {
        let table = StringTable::new();
        // Quoted placeholder must still be skipped, malformed placeholder must not.
        let cwt = r#"localisation_commands = { "<scripted_loc>" = any "<scripted_loc" = any "<scripted_loc> " = any GetFoo = any }"#;
        let parsed = parse_string(cwt, &table);
        let rs = ast_to_ruleset(&parsed, &table);
        assert!(
            !rs.localisation_commands.contains("<scripted_loc>"),
            "quoted placeholder must be skipped"
        );
        // "<scripted_loc" is not a well-formed placeholder (missing '>'), so it is kept as a name — but lowercased, unlikely to collide.
        assert!(rs.localisation_commands.contains("<scripted_loc"));
        assert!(rs.localisation_commands.contains("getfoo"));
        assert_eq!(rs.localisation_commands.len(), 3);
    }
}
