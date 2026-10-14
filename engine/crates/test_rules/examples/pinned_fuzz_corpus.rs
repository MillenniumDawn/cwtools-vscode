use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

fn main() {
    let destination = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("usage: cargo run -p cwtools_test_rules --example pinned_fuzz_corpus -- <destination>");
            std::process::exit(2);
        });
    let count = stage_pinned_fuzz_corpus(&destination).unwrap_or_else(|error| {
        panic!(
            "cannot stage pinned rules corpus at {}: {error}",
            destination.display()
        )
    });
    println!(
        "staged {count} pinned upstream .cwt files in {}",
        destination.display()
    );
}

fn stage_pinned_fuzz_corpus(destination: &Path) -> std::io::Result<usize> {
    stage_cwt_corpus(
        cwtools_test_rules::stellaris_config(),
        cwtools_test_rules::hoi4_config(),
        destination,
    )
}

fn stage_cwt_corpus(stellaris: &Path, hoi4: &Path, destination: &Path) -> std::io::Result<usize> {
    ensure_empty_destination(destination)?;
    let stellaris_files = collect_cwt_files(stellaris)?;
    let hoi4_files = collect_cwt_files(hoi4)?;
    if stellaris_files.is_empty() || hoi4_files.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "pinned rules corpus must include CWT files for both games (Stellaris: {}, HOI4: {})",
                stellaris_files.len(),
                hoi4_files.len()
            ),
        ));
    }
    std::fs::create_dir_all(destination)?;
    copy_cwt_files("stellaris", &stellaris_files, destination)?;
    copy_cwt_files("hoi4", &hoi4_files, destination)?;
    Ok(stellaris_files.len() + hoi4_files.len())
}

fn ensure_empty_destination(destination: &Path) -> std::io::Result<()> {
    if destination.exists() && std::fs::read_dir(destination)?.next().is_some() {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            format!("corpus destination is not empty: {}", destination.display()),
        ));
    }
    Ok(())
}

fn collect_cwt_files(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_cwt_files_into(directory, &mut files)?;
    Ok(files)
}

fn collect_cwt_files_into(directory: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_cwt_files_into(&entry.path(), files)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cwt"))
        {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn copy_cwt_files(game: &str, files: &[PathBuf], destination: &Path) -> std::io::Result<()> {
    for (index, source_file) in files.iter().enumerate() {
        let name = format!("{game}-{index:04}.cwt");
        std::fs::copy(source_file, destination.join(name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::stage_cwt_corpus;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn staged_bytes(directory: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    fn sources(root: &Path) -> (PathBuf, PathBuf) {
        (root.join("stellaris"), root.join("hoi4"))
    }

    #[test]
    fn pinned_seed_staging_is_complete_deterministic_and_namespaced() {
        let source = tempfile::tempdir().unwrap();
        let (stellaris, hoi4) = sources(source.path());
        write(&stellaris.join("common/z.cwt"), "z = yes");
        write(&stellaris.join("a.cwt"), "a = yes");
        write(&stellaris.join("ignored.txt"), "not a seed");
        write(&hoi4.join("alerts.cwt"), "alert = yes");
        let destination = tempfile::tempdir().unwrap();
        let repeated = tempfile::tempdir().unwrap();

        assert_eq!(
            stage_cwt_corpus(&stellaris, &hoi4, destination.path()).unwrap(),
            3
        );
        assert_eq!(
            stage_cwt_corpus(&stellaris, &hoi4, repeated.path()).unwrap(),
            3
        );
        let staged = staged_bytes(destination.path());
        assert_eq!(staged, staged_bytes(repeated.path()));
        assert_eq!(staged.len(), 3);
        assert_eq!(staged[0].1, b"alert = yes");
        assert_eq!(staged[1].1, b"a = yes");
        assert_eq!(staged[2].1, b"z = yes");
        assert!(
            staged
                .iter()
                .any(|(name, _)| name.starts_with("stellaris-"))
        );
        assert!(staged.iter().any(|(name, _)| name.starts_with("hoi4-")));
    }

    #[test]
    fn staging_rejects_nonempty_destination_without_touching_it() {
        let source = tempfile::tempdir().unwrap();
        let (stellaris, hoi4) = sources(source.path());
        write(&stellaris.join("rules.cwt"), "source = yes");
        write(&hoi4.join("rules.cwt"), "source = yes");
        let destination = tempfile::tempdir().unwrap();
        let existing = destination.path().join("old-seed.cwt");
        write(&existing, "keep me");

        let error = stage_cwt_corpus(&stellaris, &hoi4, destination.path()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(existing).unwrap(), b"keep me");
        assert_eq!(std::fs::read_dir(destination.path()).unwrap().count(), 1);
    }

    #[test]
    fn staging_rejects_a_game_without_cwt_files() {
        for empty_game in ["stellaris", "hoi4"] {
            let source = tempfile::tempdir().unwrap();
            let (stellaris, hoi4) = sources(source.path());
            let (nonempty, empty) = if empty_game == "stellaris" {
                (&hoi4, &stellaris)
            } else {
                (&stellaris, &hoi4)
            };
            write(&nonempty.join("rules.cwt"), "source = yes");
            std::fs::create_dir_all(empty).unwrap();
            let destination = tempfile::tempdir().unwrap();

            let error = stage_cwt_corpus(&stellaris, &hoi4, destination.path()).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData, "{empty_game}");
            assert_eq!(std::fs::read_dir(destination.path()).unwrap().count(), 0);
        }
    }
}
