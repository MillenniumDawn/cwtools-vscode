//! `fix`: apply (or preview) the machine-applicable fixes the validators emit.

use cwtools_driver::{RulesInput, Session, SessionConfig, VanillaCacheAuto};
use cwtools_game::constants::Game;
use cwtools_info::vanilla_cache;
use cwtools_rules::ruleset_loader::RuleParseError;
use std::collections::BTreeMap;

use crate::cli::FixArgs;
use crate::run::{
    EXIT_DISCOVERY_FAILED, announce_config, exit_if_empty, load_config, missing_required,
};
use crate::{codes, config};

/// A fix to apply to one file: the diagnostic code (for the skip warning) paired
/// with the underlying edit. Grouped per file by the `fix` subcommand and handed
/// to `cwtools_parser::fix::plan_file_edits`, which owns the overlap resolution
/// the LSP `source.fixAll` action shares.
type PlannedFix = (String, cwtools_parser::fix::SpanEdit);

pub(super) fn run(args: FixArgs) {
    let FixArgs {
        config,
        game,
        directory,
        rules,
        vanilla,
        vanilla_cache,
        no_vanilla_cache,
        refresh_vanilla_cache,
        ignore_files,
        ignore_dirs,
        loc_language,
        case_sensitive_files,
        codes: only_flag,
        apply,
        allow_empty,
    } = args;

    let file_cfg = load_config(config.as_deref(), directory.as_deref());
    let mut applied: Vec<&'static str> = Vec::new();
    let fc = file_cfg.as_ref();
    let game = config::pick(game, fc.and_then(|c| c.game.clone()), "game", &mut applied);
    let directory = config::pick(
        directory,
        fc.and_then(|c| c.directory.clone()),
        "directory",
        &mut applied,
    );
    let rules = config::pick(
        rules,
        fc.and_then(|c| c.rules.clone()),
        "rules",
        &mut applied,
    );
    let vanilla = config::pick(
        vanilla,
        fc.and_then(|c| c.vanilla.clone()),
        "vanilla",
        &mut applied,
    );
    let vanilla_cache = config::pick(
        vanilla_cache,
        fc.and_then(|c| c.vanilla_cache.clone()),
        "vanilla-cache",
        &mut applied,
    );
    let no_vanilla_cache = config::pick_flag(
        no_vanilla_cache,
        fc.is_some_and(|c| c.no_vanilla_cache),
        "no-vanilla-cache",
        &mut applied,
    );
    let refresh_vanilla_cache = config::pick_flag(
        refresh_vanilla_cache,
        fc.is_some_and(|c| c.refresh_vanilla_cache),
        "refresh-vanilla-cache",
        &mut applied,
    );
    let case_sensitive_files = config::pick_flag_default(
        case_sensitive_files,
        fc.and_then(|c| c.case_sensitive_files),
        "case-sensitive-files",
        &mut applied,
    );
    let ignore_files = config::pick_list(
        ignore_files,
        fc.map(|c| c.ignore_files.clone()).unwrap_or_default(),
        "ignore-files",
        &mut applied,
    );
    let ignore_dirs = config::pick_list(
        ignore_dirs,
        fc.map(|c| c.ignore_dirs.clone()).unwrap_or_default(),
        "ignore-dirs",
        &mut applied,
    );
    let loc_language = config::pick_list(
        loc_language,
        fc.map(|c| c.loc_languages.clone()).unwrap_or_default(),
        "loc-languages",
        &mut applied,
    );
    // `--code` is `fix`'s own spelling of the config's `only-codes`. It
    // predates the validated code flags and stays lenient: an unknown
    // code warns rather than failing the run.
    let only_flag: Vec<String> = only_flag
        .iter()
        .map(|c| c.to_ascii_uppercase())
        .inspect(|c| {
            if codes::entry(c).is_none() {
                eprintln!("warn: --code {c} is not a code the validator emits");
            }
        })
        .collect();
    let only_codes = config::pick_list(
        only_flag,
        fc.map(|c| c.only_codes.clone()).unwrap_or_default(),
        "only-codes",
        &mut applied,
    );
    let ignore_codes = config::pick_list(
        Vec::new(),
        fc.map(|c| c.ignore_codes.clone()).unwrap_or_default(),
        "ignore-codes",
        &mut applied,
    );
    let allow_empty = config::pick_flag(
        allow_empty,
        fc.is_some_and(|c| c.allow_empty),
        "allow-empty",
        &mut applied,
    );
    announce_config("fix", fc, &applied, config::FIX_KEYS);

    let game = game.unwrap_or_else(|| missing_required("fix", "--game <GAME>", "game", fc));
    let directory = directory
        .unwrap_or_else(|| missing_required("fix", "--directory <DIRECTORY>", "directory", fc));
    let rules = rules.unwrap_or_else(|| missing_required("fix", "--rules <RULES>", "rules", fc));

    let game_id = Game::from_str(&game).unwrap_or_else(|| {
        eprintln!("Unknown game: {}. Supported: hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, eu5, custom", game);
        std::process::exit(1);
    });

    let want = |code: &str| codes::wanted(code, &only_codes, &ignore_codes);

    let vanilla_cache_index = vanilla_cache
        .as_ref()
        .and_then(|p| match vanilla_cache::load(p) {
            Ok(loaded) => Some(loaded),
            Err(e) => {
                eprintln!(
                    "  warn: could not load vanilla cache {}: {}",
                    p.display(),
                    e
                );
                None
            }
        })
        .map(|(_, fp, data)| (fp, data));
    let (_fp, vanilla_cache_index) = vanilla_cache_index.unzip();

    // Same automatic base-game cache as `validate`, so both commands see
    // the same base-game data (and share the warm cache).
    let vanilla_cache_auto = if no_vanilla_cache || vanilla_cache.is_some() {
        None
    } else {
        cwtools_driver::default_cache_dir().map(|dir| VanillaCacheAuto {
            dir,
            refresh: refresh_vanilla_cache,
        })
    };

    let session = Session::load_with_parse_cache(
        SessionConfig {
            game: game_id,
            rules: RulesInput::from_path(rules.clone()),
            directory: directory.clone(),
            vanilla: vanilla.clone(),
            vanilla_cache: vanilla_cache_index,
            vanilla_cache_auto,
            ignore_files: &ignore_files,
            ignore_dirs: &ignore_dirs,
            loc_languages: if loc_language.is_empty() {
                None
            } else {
                Some(loc_language)
            },
            case_sensitive_files,
            on_rules_diagnostic: Some(&mut |e: RuleParseError| eprintln!("warn: {}", e)),
        },
        cwtools_driver::default_cache_dir(),
    );

    // Same guards as `validate`: a failed walk, an empty ruleset, or an
    // empty file set must not read as "nothing needed fixing".
    if session.discovery_failed {
        std::process::exit(EXIT_DISCOVERY_FAILED);
    }
    exit_if_empty(
        session.ruleset().types.len(),
        allow_empty,
        "--rules loaded 0 types",
        &rules,
    );
    exit_if_empty(
        session.parsed_files().len(),
        allow_empty,
        "--directory contains no files to fix",
        &directory,
    );

    // Gather fixable diagnostics, grouped per file in deterministic order.
    let mut by_file: BTreeMap<String, Vec<PlannedFix>> = BTreeMap::new();
    for (path, errors) in session.validate_all() {
        let file_str = path.to_str().unwrap_or("").to_string();
        for err in errors {
            let code = err.code.unwrap_or_default();
            if !want(code) {
                continue;
            }
            if let Some(fix) = err.fix {
                for edit in fix.edits {
                    by_file
                        .entry(file_str.clone())
                        .or_default()
                        .push((code.to_string(), edit));
                }
            }
        }
    }
    // Loc diagnostics: only mod-path files (mirror `validate`'s filter).
    let dir_prefix = {
        let s = directory.to_string_lossy();
        if s.ends_with(std::path::MAIN_SEPARATOR) {
            s.into_owned()
        } else {
            format!("{}{}", s, std::path::MAIN_SEPARATOR)
        }
    };
    for d in session.loc_project_diagnostics() {
        if !d.file.starts_with(&dir_prefix) || !want(d.code) {
            continue;
        }
        if let Some(fix) = d.fix {
            for edit in fix.edits {
                by_file
                    .entry(d.file.clone())
                    .or_default()
                    .push((d.code.to_string(), edit));
            }
        }
    }

    let mut files_changed = 0usize;
    let mut edits_applied = 0usize;
    let mut write_failed = false;
    for (file, planned) in by_file {
        let Ok(text) = std::fs::read_to_string(&file) else {
            eprintln!("warn: could not read {file}; skipping its fixes");
            continue;
        };
        let (kept, skipped) = cwtools_parser::fix::plan_file_edits(&text, planned);
        for code in &skipped {
            eprintln!("warn: {file}: skipped a {code} fix (overlaps another edit)");
        }
        if kept.is_empty() {
            continue;
        }
        if apply {
            let fixed = cwtools_parser::fix::apply_edits(&text, &kept);
            if let Err(e) = std::fs::write(&file, &fixed) {
                eprintln!("Error writing {file}: {e}");
                write_failed = true;
            } else {
                files_changed += 1;
                edits_applied += kept.len();
                println!("fixed {file} ({} edit(s))", kept.len());
            }
        } else {
            print!("{}", fix_preview(&file, &text, &kept));
            files_changed += 1;
            edits_applied += kept.len();
        }
    }

    if apply {
        println!(
            "\nApplied {} fix(es) across {} file(s)",
            edits_applied, files_changed
        );
    } else {
        println!(
            "\nDry run: {} fix(es) across {} file(s) would be applied (pass --apply to write)",
            edits_applied, files_changed
        );
    }

    if write_failed {
        std::process::exit(2);
    }
}

/// A unified-diff-style preview of applying `edits` to `old` under `path`. One
/// hunk per edit (edits are already non-overlapping), showing the touched old
/// lines (`-`) and the resulting new lines (`+`).
fn fix_preview(path: &str, old: &str, edits: &[cwtools_parser::fix::SpanEdit]) -> String {
    use cwtools_parser::fix::{line_start_bytes, pos_to_byte};
    let starts = line_start_bytes(old);
    let line_of = |byte: usize| match starts.binary_search(&byte) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let mut resolved: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            (
                pos_to_byte(old, &starts, edit.range.start),
                pos_to_byte(old, &starts, edit.range.end),
                edit.replacement.as_str(),
            )
        })
        .collect();
    resolved.sort_by_key(|r| r.0);

    let mut out = format!("--- {path}\n+++ {path}\n");
    for (s, e, repl) in resolved {
        let start_line = line_of(s);
        let end_line = if e > s { line_of(e - 1) } else { start_line };
        let hunk_start = starts[start_line];
        let hunk_end = starts.get(end_line + 1).copied().unwrap_or(old.len());
        let old_seg = &old[hunk_start..hunk_end];
        let new_seg = format!("{}{}{}", &old[hunk_start..s], repl, &old[e..hunk_end]);
        out.push_str(&format!("@@ -{} +{} @@\n", start_line + 1, start_line + 1));
        for l in old_seg.split_inclusive('\n') {
            out.push_str(&format!("-{}\n", l.strip_suffix('\n').unwrap_or(l)));
        }
        for l in new_seg.split_inclusive('\n') {
            out.push_str(&format!("+{}\n", l.strip_suffix('\n').unwrap_or(l)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::ast::{SourcePos, SourceRange};
    use cwtools_parser::fix::SpanEdit;

    fn edit(l0: u32, c0: u16, l1: u32, c1: u16, repl: &str) -> SpanEdit {
        SpanEdit {
            range: SourceRange {
                start: SourcePos { line: l0, col: c0 },
                end: SourcePos { line: l1, col: c1 },
            },
            replacement: repl.to_string(),
        }
    }

    // Step 5: multi-edit-per-file ordering. Two non-overlapping edits on one file
    // apply to the same result regardless of the order they were queued.
    #[test]
    fn multiple_edits_per_file_apply_in_descending_order() {
        let text = "aaaa bbbb\n";
        let forward: Vec<PlannedFix> = vec![
            ("CWA".into(), edit(1, 0, 1, 4, "X")),
            ("CWB".into(), edit(1, 5, 1, 9, "Y")),
        ];
        let reversed: Vec<PlannedFix> = vec![
            ("CWB".into(), edit(1, 5, 1, 9, "Y")),
            ("CWA".into(), edit(1, 0, 1, 4, "X")),
        ];
        for planned in [forward, reversed] {
            let (kept, skipped) = cwtools_parser::fix::plan_file_edits(text, planned);
            assert!(skipped.is_empty(), "no overlap expected");
            assert_eq!(kept.len(), 2);
            assert_eq!(cwtools_parser::fix::apply_edits(text, &kept), "X Y\n");
        }
    }

    // Step 5: overlap skip. When two edits overlap, the later one is dropped (and
    // reported) so it can't corrupt the kept edit.
    #[test]
    fn overlapping_edits_skip_and_warn() {
        let text = "aaaa bbbb\n";
        let planned: Vec<PlannedFix> = vec![
            ("CWA".into(), edit(1, 0, 1, 6, "X")), // covers "aaaa b"
            ("CWB".into(), edit(1, 5, 1, 9, "Y")), // overlaps at col 5
        ];
        let (kept, skipped) = cwtools_parser::fix::plan_file_edits(text, planned);
        assert_eq!(kept.len(), 1, "one edit kept");
        assert_eq!(skipped, vec!["CWB".to_string()], "overlapping edit skipped");
        assert_eq!(cwtools_parser::fix::apply_edits(text, &kept), "Xbbb\n");
    }
}
