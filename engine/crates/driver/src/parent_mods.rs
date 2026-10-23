use std::path::{Path, PathBuf};

use cwtools_file_manager::{ResolvedMod, expand_multiple_mods, read_text};
use cwtools_parser::ast::{Child, Value};
use cwtools_parser::parser::parse_string_without_comments;
use cwtools_parser::unquote;
use cwtools_string_table::string_table::StringTable;

/// Resolve configured parent roots in load order, returning a warning for each
/// entry that cannot be used or is excluded.
pub fn resolve_parent_roots<P: AsRef<Path>>(
    entries: &[P],
    workspace_root: Option<&Path>,
    vanilla_dir: Option<&Path>,
) -> (Vec<PathBuf>, Vec<String>) {
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    let workspace = workspace_root.and_then(canonical);
    let vanilla = vanilla_dir.and_then(canonical);
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut dropped = Vec::new();
    for entry in entries {
        let raw = entry.as_ref();
        let path = match workspace_root {
            Some(root) if raw.is_relative() => root.join(raw),
            _ => raw.to_path_buf(),
        };
        let Some(real) = canonical(&path).filter(|real| real.is_dir()) else {
            dropped.push(format!("parent mod is not a directory: {}", raw.display()));
            continue;
        };
        let reason = if workspace
            .as_ref()
            .is_some_and(|ws| ws.starts_with(&real) || real.starts_with(ws))
        {
            Some("overlaps the workspace root")
        } else if vanilla.as_ref() == Some(&real) {
            Some("is the base-game dir")
        } else if kept.iter().any(|(_, seen)| *seen == real) {
            Some("is listed twice")
        } else {
            None
        };
        match reason {
            Some(reason) => dropped.push(format!("parent mod {reason}: {}", raw.display())),
            None => kept.push((path, real)),
        }
    }
    (kept.into_iter().map(|(path, _)| path).collect(), dropped)
}

/// Resolve `descriptor.mod` dependencies from launcher `.mod` files. Passing
/// `launcher_user_dir` bypasses OS home-directory discovery, for deterministic
/// callers and tests; `None` uses the existing per-game OS location policy.
pub fn discover_parent_roots(
    workspace: &Path,
    language: &str,
    vanilla_dir: Option<&Path>,
    launcher_user_dir: Option<&Path>,
) -> (Vec<PathBuf>, Vec<String>) {
    let user_dir = launcher_user_dir
        .map(Path::to_path_buf)
        .or_else(|| user_data_dir(language));
    match user_dir {
        Some(user_dir) => discover_in(workspace, &user_dir, vanilla_dir),
        None => (Vec::new(), Vec::new()),
    }
}

fn discover_in(
    workspace: &Path,
    user_dir: &Path,
    vanilla_dir: Option<&Path>,
) -> (Vec<PathBuf>, Vec<String>) {
    let dependencies = descriptor_dependencies(workspace);
    if dependencies.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let launcher_mods = expand_multiple_mods(user_dir);
    resolve_dependencies(workspace, vanilla_dir, &dependencies, &launcher_mods)
}

fn user_data_dir(language: &str) -> Option<PathBuf> {
    let game = match language {
        "hoi4" => "Hearts of Iron IV",
        "stellaris" => "Stellaris",
        "eu4" => "Europa Universalis IV",
        "eu5" => "Europa Universalis V",
        "ck2" => "Crusader Kings II",
        "ck3" => "Crusader Kings III",
        "vic3" => "Victoria 3",
        _ => return None,
    };
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)?;
    let documents = if cfg!(target_os = "linux") {
        ".local/share"
    } else {
        "Documents"
    };
    Some(home.join(documents).join("Paradox Interactive").join(game))
}

fn descriptor_dependencies(workspace: &Path) -> Vec<String> {
    let Ok(source) = read_text(&workspace.join("descriptor.mod")) else {
        return Vec::new();
    };
    let table = StringTable::new();
    let parsed = parse_string_without_comments(&source, &table);
    let arena = &parsed.arena;
    parsed
        .root_children
        .iter()
        .filter_map(|child| arena.keyed_clause(child))
        .filter(|clause| table.get_string(clause.key.normal).as_deref() == Some("dependencies"))
        .flat_map(|clause| clause.children)
        .filter_map(|child| match child {
            Child::LeafValue(index) => Some(&arena.leaf_values[*index as usize].value),
            _ => None,
        })
        .filter_map(|value| match value {
            Value::String(tokens) | Value::QString(tokens) => table.get_string(tokens.normal),
            _ => None,
        })
        .map(|raw| unquote(&raw).to_string())
        .collect()
}

fn resolve_dependencies(
    workspace: &Path,
    vanilla_dir: Option<&Path>,
    dependencies: &[String],
    launcher_mods: &[ResolvedMod],
) -> (Vec<PathBuf>, Vec<String>) {
    let mut entries: Vec<PathBuf> = Vec::new();
    let mut warnings = Vec::new();
    for name in dependencies {
        // Several launcher entries can point at one directory; they count once.
        let mut distinct: Vec<(PathBuf, &Path)> = Vec::new();
        for item in launcher_mods
            .iter()
            .filter(|item| item.descriptor.name == *name)
        {
            if let Ok(real) = std::fs::canonicalize(&item.root)
                && !distinct.iter().any(|(seen, _)| *seen == real)
            {
                distinct.push((real, &item.root));
            }
        }
        match distinct.as_slice() {
            [] => warnings.push(format!("parent mod dependency not found: {name}")),
            [(_, root)] => entries.push(root.to_path_buf()),
            _ => {
                let roots = distinct
                    .iter()
                    .map(|(_, root)| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                warnings.push(format!(
                    "parent mod dependency is ambiguous: {name} matches {roots}; skipping"
                ));
            }
        }
    }
    let (roots, dropped) = resolve_parent_roots(&entries, Some(workspace), vanilla_dir);
    warnings.extend(dropped);
    (roots, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_file_manager::ModDescriptor;

    fn make_dirs<const N: usize>(base: &Path, names: [&str; N]) -> [PathBuf; N] {
        names.map(|name| {
            let path = base.join(name);
            std::fs::create_dir(&path).unwrap();
            path
        })
    }

    fn launcher_mod(name: &str, root: &Path) -> ResolvedMod {
        ResolvedMod {
            descriptor: ModDescriptor {
                name: name.into(),
                path: None,
                replace_paths: Vec::new(),
            },
            root: root.to_path_buf(),
        }
    }

    #[test]
    fn reads_quoted_bare_and_cp1252_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("descriptor.mod"),
            b"name = Submod\ndependencies = {\n  # comment\n  \"First Parent\"\n  Second\n  \"Caf\xE9\"\n}\n",
        )
        .unwrap();
        assert_eq!(
            descriptor_dependencies(dir.path()),
            ["First Parent", "Second", "Caf\u{e9}"]
        );
    }

    #[test]
    fn resolves_found_missing_and_ambiguous_names() {
        let dir = tempfile::tempdir().unwrap();
        let [workspace, found, shared, first, second] = make_dirs(
            dir.path(),
            ["workspace", "found", "shared", "first", "second"],
        );
        let launcher_mods = [
            launcher_mod("Found", &found),
            launcher_mod("Shared", &shared),
            launcher_mod("Shared", &shared),
            launcher_mod("Ambiguous", &first),
            launcher_mod("Ambiguous", &second),
        ];
        let names = ["Found", "Shared", "Ambiguous", "Missing"].map(String::from);

        let (roots, warnings) = resolve_dependencies(&workspace, None, &names, &launcher_mods);

        assert_eq!(roots, [found, shared]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("ambiguous: Ambiguous"), "{warnings:?}");
        assert!(warnings[1].contains("not found: Missing"), "{warnings:?}");
    }

    #[cfg(unix)]
    #[test]
    fn one_directory_under_two_spellings_resolves_once_as_the_first_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let [workspace, real] = make_dirs(dir.path(), ["workspace", "real"]);
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let launcher_mods = [
            launcher_mod("Parent", &alias),
            launcher_mod("Parent", &real),
        ];

        let (roots, warnings) =
            resolve_dependencies(&workspace, None, &["Parent".to_string()], &launcher_mods);

        assert_eq!(roots, [alias]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn discovers_launcher_parents_in_dependency_order_with_injected_user_dir() {
        let dir = tempfile::tempdir().unwrap();
        let [workspace, user, absolute] =
            make_dirs(dir.path(), ["workspace", "user", "absolute-parent"]);
        let relative = user.join("mod/relative-parent");
        std::fs::create_dir_all(&relative).unwrap();
        std::fs::write(
            workspace.join("descriptor.mod"),
            "name=Submod\ndependencies={ \"Relative Parent\" \"Absolute Parent\" }\n",
        )
        .unwrap();
        std::fs::write(
            user.join("mod/absolute.mod"),
            format!(
                "name=\"Absolute Parent\"\npath=\"{}\"\n",
                absolute.display()
            ),
        )
        .unwrap();
        std::fs::write(
            user.join("mod/relative.mod"),
            "name=\"Relative Parent\"\npath=\"mod/relative-parent\"\n",
        )
        .unwrap();

        let (roots, warnings) = discover_parent_roots(&workspace, "hoi4", None, Some(&user));

        assert_eq!(roots, [relative, absolute]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn resolve_parent_roots_keeps_distinct_dirs_in_load_order() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let dir = |name: &str| {
            let path = tmp.path().join(name);
            std::fs::create_dir_all(&path).expect("mkdir");
            path
        };
        let workspace = dir("mods/submod");
        let nested = dir("mods/submod/inner");
        let vanilla = dir("game");
        let first = dir("mods/base");
        let second = dir("mods/compat");
        let text = |path: &Path| path.to_string_lossy().into_owned();
        let entries = vec![
            text(&second),
            "../base".to_string(),
            text(&second),
            text(&tmp.path().join("mods")),
            text(&nested),
            text(&vanilla),
            text(&tmp.path().join("missing")),
        ];

        let (kept, dropped) = resolve_parent_roots(&entries, Some(&workspace), Some(&vanilla));

        assert_eq!(kept, vec![second.clone(), workspace.join("../base")]);
        assert_eq!(
            std::fs::canonicalize(&kept[1]).unwrap(),
            std::fs::canonicalize(&first).unwrap()
        );
        assert_eq!(dropped.len(), 5, "{dropped:?}");
        assert!(dropped[0].contains("listed twice"), "{dropped:?}");
        assert!(dropped[1].contains("overlaps the workspace root"));
        assert!(dropped[2].contains("overlaps the workspace root"));
        assert!(dropped[3].contains("base-game"));
        assert!(dropped[4].contains("not a directory"));
    }
}
