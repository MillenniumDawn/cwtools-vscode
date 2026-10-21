use std::path::{Path, PathBuf};

use cwtools_file_manager::{ResolvedMod, expand_multiple_mods, read_text};
use cwtools_parser::ast::{Child, Value};
use cwtools_parser::parser::parse_string_without_comments;
use cwtools_parser::unquote;
use cwtools_string_table::string_table::StringTable;

use crate::config::resolve_parent_roots;

/// Resolve the workspace's `descriptor.mod` dependencies against the launcher's
/// `.mod` files, for when `cwtools.parentMods` is empty (#818). Returns the
/// parent roots in dependency order and one warning per dependency skipped.
pub(crate) fn discover_parent_roots(
    workspace: &Path,
    language: &str,
    vanilla_dir: Option<&Path>,
) -> (Vec<PathBuf>, Vec<String>) {
    match user_data_dir(language) {
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

/// The names in the workspace descriptor's `dependencies = { ... }` clause.
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
    let mut entries = Vec::new();
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
            [(_, root)] => {
                tracing::info!(dependency = %name, root = %root.display(), "resolved parent mod");
                entries.push(root.to_string_lossy().into_owned());
            }
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
    fn discovers_launcher_parents_in_dependency_order() {
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

        let (roots, warnings) = discover_in(&workspace, &user, None);

        assert_eq!(roots, [relative, absolute]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
