//! `format`: reprint script files with normalized whitespace.

use cwtools_parser::format::{FormatOptions, IndentStyle, format_text};
use cwtools_string_table::string_table::StringTable;
use std::path::PathBuf;

use crate::cli::FormatArgs;
use crate::run::{
    EXIT_DISCOVERY_FAILED, announce_config, exit_if_empty, load_config, missing_required,
};

pub(super) fn run(args: FormatArgs) {
    let FormatArgs {
        config,
        directory,
        ignore_files,
        ignore_dirs,
        indent_style,
        indent_size,
        apply,
        allow_empty,
    } = args;

    let file_cfg = load_config(config.as_deref(), directory.as_deref());
    let mut applied: Vec<&'static str> = Vec::new();
    let fc = file_cfg.as_ref();
    let directory = crate::config::pick(
        directory,
        fc.and_then(|c| c.directory.clone()),
        "directory",
        &mut applied,
    );
    let ignore_files = crate::config::pick_list(
        ignore_files,
        fc.map(|c| c.ignore_files.clone()).unwrap_or_default(),
        "ignore-files",
        &mut applied,
    );
    let ignore_dirs = crate::config::pick_list(
        ignore_dirs,
        fc.map(|c| c.ignore_dirs.clone()).unwrap_or_default(),
        "ignore-dirs",
        &mut applied,
    );
    let allow_empty = crate::config::pick_flag(
        allow_empty,
        fc.is_some_and(|c| c.allow_empty),
        "allow-empty",
        &mut applied,
    );
    announce_config("format", fc, &applied, crate::config::FORMAT_KEYS);

    let directory: PathBuf = directory
        .unwrap_or_else(|| missing_required("format", "--directory <DIRECTORY>", "directory", fc));

    let indent_style = match indent_style.as_str() {
        "space" => IndentStyle::Space,
        "tab" => IndentStyle::Tab,
        other => {
            eprintln!("error: invalid --indent-style '{other}': expected space or tab");
            std::process::exit(2);
        }
    };
    let opts = FormatOptions {
        indent_style,
        indent_size: indent_size.clamp(1, 16),
        ..FormatOptions::default()
    };

    let mut fm_config = cwtools_driver::search_config_for(&directory);
    fm_config
        .exclude_patterns
        .extend(ignore_files.iter().cloned());
    fm_config
        .exclude_dir_patterns
        .extend(ignore_dirs.iter().cloned());
    let files = match cwtools_driver::discover_workspace_files(fm_config) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("error: discovery failed for {}: {e}", directory.display());
            std::process::exit(EXIT_DISCOVERY_FAILED);
        }
    };
    exit_if_empty(
        files.len(),
        allow_empty,
        "--directory contains no files to format",
        &directory,
    );

    let table = StringTable::new();
    let mut files_changed = 0usize;
    let mut skipped = 0usize;
    let mut write_failed = false;
    for file in &files {
        let path = &file.path;
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("warn: could not read {}; skipping", path.display());
            skipped += 1;
            continue;
        };
        match format_text(&text, &table, &opts) {
            None => {
                eprintln!("warn: {} failed to parse; skipping", path.display());
                skipped += 1;
            }
            Some(formatted) if formatted == text => {}
            Some(formatted) => {
                if apply {
                    if let Err(e) = std::fs::write(path, &formatted) {
                        eprintln!("Error writing {}: {e}", path.display());
                        write_failed = true;
                    } else {
                        files_changed += 1;
                        println!("formatted {}", path.display());
                    }
                } else {
                    files_changed += 1;
                    print!(
                        "{}",
                        unified_diff(&path.display().to_string(), &text, &formatted)
                    );
                }
            }
        }
    }

    if apply {
        println!("\nFormatted {files_changed} file(s)");
    } else {
        println!("\nDry run: {files_changed} file(s) would be formatted (pass --apply to write)");
    }
    if skipped > 0 {
        eprintln!("skipped {skipped} file(s) (unreadable or parse errors)");
    }
    if write_failed {
        std::process::exit(2);
    }
    if !apply && files_changed > 0 {
        std::process::exit(1);
    }
}

fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut out = format!(
        "--- {path}\n+++ {path}\n@@ -1,{} +1,{} @@\n",
        old_lines.len(),
        new_lines.len()
    );
    for line in old_lines {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in new_lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}
