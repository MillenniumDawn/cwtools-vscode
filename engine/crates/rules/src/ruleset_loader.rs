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

#[derive(Debug, Clone)]
pub struct RuleParseError {
    pub file: std::path::PathBuf,
    pub line: u32,
    pub col: u16,
    pub code: &'static str,
    pub severity: ErrorSeverity,
    pub message: String,
}

impl RuleParseError {
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

fn parse_folders_list(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

struct FileRules {
    ruleset: RuleSet,
    ref_candidates: Vec<crate::config_validation::RefCandidate>,
    def_positions: Vec<CwtDefPosition>,
    errors: Vec<RuleParseError>,
}

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
    let mut ref_candidates: Vec<crate::config_validation::RefCandidate> = Vec::new();
    for file in per_file {
        errors.extend(file.errors);
        merge_ruleset(&mut combined, file.ruleset);
        ref_candidates.extend(file.ref_candidates);
        combined.def_positions.extend(file.def_positions);
    }

    let refused = post_process(&mut combined);
    errors.extend(
        refused
            .into_iter()
            .map(|error| alias_expansion_error(dir, &combined, error)),
    );

    combined.reindex();

    errors.extend(crate::config_validation::resolve_reference_candidates(
        &ref_candidates,
        &combined,
    ));

    (combined, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn duplicate_types_keep_the_first_definition_and_rule() {
        let table = StringTable::new();
        let parsed = parse_string(
            r#"types = {
    type[thing] = { path = "first" }
    type[thing] = { path = "second" }
}
thing = { first = any }
thing = { second = any }
"#,
            &table,
        );
        let ruleset = ast_to_ruleset(&parsed, &table);

        assert_eq!(ruleset.types.len(), 2);
        assert_eq!(ruleset.type_by_name().get("thing"), Some(&0));
        assert_eq!(ruleset.types[0].path_options.paths, vec!["first"]);
        assert_eq!(ruleset.type_rules_idx().get("thing"), Some(&0));
    }

    #[test]
    fn duplicate_enums_in_one_file_union_values_in_source_order() {
        let table = StringTable::new();
        let parsed = parse_string(
            r#"enums = {
    ### first description
    enum[shared] = { FIRST Shared }
    ### second description
    enum[shared] = { shared second }
}
"#,
            &table,
        );
        let ruleset = ast_to_ruleset(&parsed, &table);

        assert_eq!(ruleset.enums.len(), 1);
        assert_eq!(ruleset.enums[0].description, "first description");
        assert_eq!(ruleset.enums[0].values, ["FIRST", "Shared", "second"]);
        assert_eq!(ruleset.enum_by_name().get("shared"), Some(&0));
    }

    #[test]
    fn duplicate_enums_union_values_in_source_order() {
        use crate::rules_types::EnumDefinition;

        let mut dst = RuleSet::new();
        dst.enums.push(EnumDefinition {
            key: "shared".to_string(),
            description: "first description".to_string(),
            values: vec!["first".to_string(), "shared".to_string()],
        });
        let mut src = RuleSet::new();
        src.enums.push(EnumDefinition {
            key: "shared".to_string(),
            description: "second description".to_string(),
            values: vec!["SHARED".to_string(), "second".to_string()],
        });

        merge_ruleset(&mut dst, src);
        dst.reindex();

        assert_eq!(dst.enums.len(), 1);
        assert_eq!(dst.enums[0].description, "first description");
        assert_eq!(dst.enums[0].values, ["first", "shared", "second"]);
        assert_eq!(dst.enum_by_name().get("shared"), Some(&0));
    }

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
        let cwt = r#"localisation_commands = { "<scripted_loc>" = any "<scripted_loc" = any "<scripted_loc> " = any GetFoo = any }"#;
        let parsed = parse_string(cwt, &table);
        let rs = ast_to_ruleset(&parsed, &table);
        assert!(
            !rs.localisation_commands.contains("<scripted_loc>"),
            "quoted placeholder must be skipped"
        );
        assert!(rs.localisation_commands.contains("<scripted_loc"));
        assert!(rs.localisation_commands.contains("getfoo"));
        assert_eq!(rs.localisation_commands.len(), 3);
    }
}
