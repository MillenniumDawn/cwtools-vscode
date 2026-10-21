use std::path::{Path, PathBuf};

use cwtools_parser::ast::{Child, Value};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;

use crate::config::resolve_parent_roots;

struct Candidate {
    name: String,
    root: PathBuf,
    descriptor: PathBuf,
}

/// Return the submod's declared dependencies in descriptor order. This uses the
/// Jomini parser so multiline clauses, comments, and quoted values work as they
/// do in game files.
fn descriptor_dependencies(path: &Path) -> Vec<String> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    let table = StringTable::new();
    let parsed = parse_string(source, &table);
    let mut dependencies = Vec::new();
    for child in &parsed.root_children {
        let Child::Leaf(index) = child else { continue };
        let leaf = &parsed.arena.leaves[*index as usize];
        if table.get_string(leaf.key.normal).as_deref() != Some("dependencies") {
            continue;
        }
        let Value::Clause(values) = &leaf.value else {
            continue;
        };
        for value in values {
            let Child::LeafValue(index) = value else {
                continue;
            };
            let value = &parsed.arena.leaf_values[*index as usize].value;
            let name = match value {
                Value::String(tokens) => table.get_string(tokens.normal),
                Value::QString(tokens) => table
                    .get_string(tokens.normal)
                    .and_then(|raw| raw.strip_prefix('"')?.strip_suffix('"').map(str::to_string)),
                _ => None,
            };
            if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                dependencies.push(name);
            }
        }
    }
    dependencies
}

fn game_metadata(language: &str) -> Option<(&'static str, &'static str)> {
    match language {
        "hoi4" => Some(("Hearts of Iron IV", "394360")),
        "stellaris" => Some(("Stellaris", "281990")),
        "eu4" => Some(("Europa Universalis IV", "236850")),
        "ck2" => Some(("Crusader Kings II", "203770")),
        "ck3" => Some(("Crusader Kings III", "1158310")),
        "vic2" => Some(("Victoria 2", "42960")),
        "vic3" => Some(("Victoria 3", "529340")),
        "ir" => Some(("ImperatorRome", "859580")),
        "eu5" => Some(("Europa Universalis V", "3450310")),
        _ => None,
    }
}

fn user_mod_dir(language: &str) -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)?;
    let (folder, _) = game_metadata(language)?;
    #[cfg(target_os = "linux")]
    let root = home.join(".local/share/Paradox Interactive");
    #[cfg(not(target_os = "linux"))]
    let root = home.join("Documents/Paradox Interactive");
    Some(root.join(folder))
}

fn steam_common_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend([
            home.join(".steam/steam/steamapps/common"),
            home.join(".local/share/Steam/steamapps/common"),
            home.join("Library/Application Support/Steam/steamapps/common"),
        ]);
    }
    dirs.extend([
        PathBuf::from("C:/Program Files (x86)/Steam/steamapps/common"),
        PathBuf::from("C:/Program Files/Steam/steamapps/common"),
    ]);
    dirs
}

fn workshop_dirs(
    language: &str,
    vanilla_dir: Option<&Path>,
    steam_common_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let Some((_, app_id)) = game_metadata(language) else {
        return Vec::new();
    };
    let mut steamapps = Vec::new();
    if let Some(vanilla) = vanilla_dir
        && let Ok(vanilla) = std::fs::canonicalize(vanilla)
        && let Some(common) = vanilla.parent()
        && let Some(root) = common.parent()
    {
        steamapps.push(root.to_path_buf());
    }
    steamapps.extend(
        steam_common_dirs
            .iter()
            .filter_map(|common| common.parent().map(Path::to_path_buf)),
    );
    steamapps
        .into_iter()
        .map(|root| root.join("workshop/content").join(app_id))
        .filter(|path| path.is_dir())
        .collect()
}

fn add_launcher_candidates(user_dir: &Path, candidates: &mut Vec<Candidate>) {
    let mod_dir = user_dir.join("mod");
    let Ok(entries) = std::fs::read_dir(mod_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let descriptor = entry.path();
        if !descriptor
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mod"))
        {
            continue;
        }
        let Some(info) = cwtools_file_manager::read_mod_descriptor(&descriptor) else {
            continue;
        };
        let Some(mod_path) = info.path.as_deref() else {
            continue;
        };
        let raw = Path::new(mod_path);
        let root = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            user_dir.join(raw)
        };
        if root.is_dir() && !info.name.is_empty() {
            candidates.push(Candidate {
                name: info.name,
                root,
                descriptor,
            });
        }
    }
}

fn candidates_for(user_dir: Option<&Path>, workshops: &[PathBuf]) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    if let Some(user_dir) = user_dir {
        add_launcher_candidates(user_dir, &mut candidates);
    }
    for workshop in workshops {
        let Ok(entries) = std::fs::read_dir(workshop) else {
            continue;
        };
        for entry in entries.flatten() {
            let root = entry.path();
            let descriptor = root.join("descriptor.mod");
            if let Some(info) = cwtools_file_manager::read_mod_descriptor(&descriptor)
                && !info.name.is_empty()
            {
                candidates.push(Candidate {
                    name: info.name,
                    root,
                    descriptor,
                });
            }
        }
    }
    candidates
}

fn resolve_dependencies(
    workspace: &Path,
    vanilla_dir: Option<&Path>,
    dependencies: &[String],
    candidates: &[Candidate],
) -> (Vec<PathBuf>, Vec<String>) {
    let mut roots = Vec::new();
    let mut messages = Vec::new();
    for name in dependencies {
        let matches: Vec<_> = candidates
            .iter()
            .filter(|item| item.name == *name)
            .collect();
        if matches.is_empty() {
            messages.push(format!("parent mod dependency not found: {name}"));
            continue;
        }
        let mut unique_roots: Vec<PathBuf> = Vec::new();
        for item in &matches {
            if let Ok(root) = std::fs::canonicalize(&item.root)
                && !unique_roots.contains(&root)
            {
                unique_roots.push(root);
            }
        }
        if matches.len() > 1 {
            let descriptors = matches
                .iter()
                .map(|item| item.descriptor.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            messages.push(format!(
                "parent mod dependency name matches multiple descriptors: {name} ({descriptors})"
            ));
        }
        if unique_roots.len() != 1 {
            let descriptors = matches
                .iter()
                .map(|item| item.descriptor.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            messages.push(format!(
                "parent mod dependency is ambiguous: {name} matches {descriptors}; skipping"
            ));
            continue;
        }
        let chosen = matches
            .iter()
            .find(|item| std::fs::canonicalize(&item.root).ok().as_ref() == unique_roots.first())
            .unwrap();
        messages.push(format!(
            "Parent mod dependency '{name}' resolved from {} to {}",
            chosen.descriptor.display(),
            chosen.root.display()
        ));
        let Some(root) = unique_roots.into_iter().next() else {
            continue;
        };
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let entries = roots
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let (roots, dropped) = resolve_parent_roots(&entries, Some(workspace), vanilla_dir);
    messages.extend(dropped);
    (roots, messages)
}

pub(crate) fn discover_parent_roots(
    workspace: &Path,
    language: &str,
    vanilla_dir: Option<&Path>,
) -> (Vec<PathBuf>, Vec<String>) {
    let steam_common_dirs = steam_common_dirs();
    discover_parent_roots_from(
        workspace,
        user_mod_dir(language).as_deref(),
        &workshop_dirs(language, vanilla_dir, &steam_common_dirs),
        vanilla_dir,
    )
}

fn discover_parent_roots_from(
    workspace: &Path,
    user_dir: Option<&Path>,
    workshops: &[PathBuf],
    vanilla_dir: Option<&Path>,
) -> (Vec<PathBuf>, Vec<String>) {
    let dependencies = descriptor_dependencies(&workspace.join("descriptor.mod"));
    let candidates = candidates_for(user_dir, workshops);
    resolve_dependencies(workspace, vanilla_dir, &dependencies, &candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_dependency_clause_with_parser() {
        let dir = tempfile::tempdir().unwrap();
        let descriptor = dir.path().join("descriptor.mod");
        std::fs::write(
            &descriptor,
            "name = Submod\ndependencies = {\n  # comment\n  \"First Parent\"\n  Second\n}\n",
        )
        .unwrap();
        assert_eq!(
            descriptor_dependencies(&descriptor),
            ["First Parent", "Second"]
        );
    }

    #[test]
    fn resolves_exact_names_and_skips_ambiguous_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("submod");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        let different = dir.path().join("developer-checkout");
        for path in [&workspace, &first, &second, &different] {
            std::fs::create_dir_all(path).unwrap();
        }
        let candidates = vec![
            Candidate {
                name: "Published Mod".into(),
                root: first.clone(),
                descriptor: first.join("descriptor.mod"),
            },
            Candidate {
                name: "Published Mod".into(),
                root: second.clone(),
                descriptor: second.join("descriptor.mod"),
            },
            Candidate {
                name: "Developer Version".into(),
                root: different,
                descriptor: dir.path().join("developer.mod"),
            },
        ];
        let (roots, messages) = resolve_dependencies(
            &workspace,
            None,
            &["Published Mod".into(), "Developer Version Alias".into()],
            &candidates,
        );
        assert!(roots.is_empty());
        assert!(
            messages
                .iter()
                .any(|m| m.contains("ambiguous: Published Mod"))
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("not found: Developer Version Alias"))
        );
    }

    #[test]
    fn published_dependency_never_falls_back_to_differently_named_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("submod");
        let published = dir.path().join("published");
        let developer = dir.path().join("developer");
        for path in [&workspace, &published, &developer] {
            std::fs::create_dir_all(path).unwrap();
        }
        let candidates = vec![
            Candidate {
                name: "Millennium Dawn: A Modern Day Mod".into(),
                root: published.clone(),
                descriptor: published.join("descriptor.mod"),
            },
            Candidate {
                name: "Millennium Dawn: Developer Version".into(),
                root: developer.clone(),
                descriptor: developer.join("descriptor.mod"),
            },
        ];
        let (roots, messages) = resolve_dependencies(
            &workspace,
            None,
            &["Millennium Dawn: A Modern Day Mod".into()],
            &candidates,
        );
        assert_eq!(roots, [std::fs::canonicalize(published).unwrap()]);
        assert!(messages.iter().any(|m| m.contains("resolved from")));
    }

    #[test]
    fn dependency_candidate_matching_base_game_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("submod");
        let vanilla = dir.path().join("game");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&vanilla).unwrap();
        let candidates = vec![Candidate {
            name: "Game Files".into(),
            root: vanilla.clone(),
            descriptor: vanilla.join("descriptor.mod"),
        }];

        let (roots, messages) = resolve_dependencies(
            &workspace,
            Some(&vanilla),
            &["Game Files".into()],
            &candidates,
        );
        assert!(roots.is_empty());
        assert!(
            messages
                .iter()
                .any(|message| { message.contains("is the base-game dir") })
        );
    }

    #[test]
    fn resolves_launcher_paths_absolute_and_relative_to_user_dir() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        let abs_root = dir.path().join("absolute-parent");
        let rel_root = user.join("mod/relative-parent");
        for path in [&user, &abs_root, &rel_root] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::create_dir_all(user.join("mod")).unwrap();
        std::fs::write(
            user.join("mod/absolute.mod"),
            format!(
                "name=\"Absolute Parent\"\npath=\"{}\"\n",
                abs_root.display()
            ),
        )
        .unwrap();
        std::fs::write(
            user.join("mod/relative.mod"),
            "name=\"Relative Parent\"\npath=\"mod/relative-parent\"\n",
        )
        .unwrap();
        let mut candidates = Vec::new();
        add_launcher_candidates(&user, &mut candidates);
        assert!(
            candidates
                .iter()
                .any(|item| item.name == "Absolute Parent" && item.root == abs_root)
        );
        assert!(
            candidates
                .iter()
                .any(|item| item.name == "Relative Parent" && item.root == rel_root)
        );
    }

    #[test]
    fn finds_workshop_descriptor_roots() {
        let dir = tempfile::tempdir().unwrap();
        let workshop = dir.path().join("394360");
        let root = workshop.join("12345");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("descriptor.mod"), "name=\"Workshop Parent\"\n").unwrap();
        let candidates = candidates_for(None, &[workshop]);
        assert!(
            candidates
                .iter()
                .any(|item| item.name == "Workshop Parent" && item.root == root)
        );
    }

    #[test]
    fn locates_steam_workshop_from_game_install() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("steamapps/common/Hearts of Iron IV");
        let workshop = dir.path().join("steamapps/workshop/content/394360");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(&workshop).unwrap();
        assert_eq!(
            workshop_dirs("hoi4", Some(&game), &[]),
            vec![std::fs::canonicalize(workshop).unwrap()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn locates_steam_workshop_through_symlinked_game_install() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir
            .path()
            .join("library/steamapps/common/Hearts of Iron IV");
        let alias = dir.path().join("game-alias");
        let workshop = dir.path().join("library/steamapps/workshop/content/394360");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(&workshop).unwrap();
        std::os::unix::fs::symlink(&game, &alias).unwrap();

        assert_eq!(
            workshop_dirs("hoi4", Some(&alias), &[]),
            vec![std::fs::canonicalize(workshop).unwrap()]
        );
    }

    #[test]
    fn resolves_dependencies_from_launcher_and_workshop_descriptors_in_load_order() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("submod");
        let user = dir.path().join("user");
        let absolute = dir.path().join("absolute-parent");
        let relative = user.join("mod/relative-parent");
        let workshops = dir.path().join("workshop/content/394360");
        let workshop_mod = workshops.join("12345");
        for path in [&workspace, &user, &absolute, &relative, &workshop_mod] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::create_dir_all(user.join("mod")).unwrap();
        std::fs::write(
            workspace.join("descriptor.mod"),
            "name=Submod\ndependencies={ \"Absolute Parent\" \"Relative Parent\" \"Workshop Parent\" }\n",
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
        std::fs::write(
            workshop_mod.join("descriptor.mod"),
            "name=\"Workshop Parent\"\n",
        )
        .unwrap();

        let (roots, messages) =
            discover_parent_roots_from(&workspace, Some(&user), &[workshops], None);
        assert_eq!(
            roots,
            [
                std::fs::canonicalize(absolute).unwrap(),
                std::fs::canonicalize(relative).unwrap(),
                std::fs::canonicalize(workshop_mod).unwrap(),
            ]
        );
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.starts_with("Parent mod dependency '"))
                .count(),
            3
        );
    }
}
