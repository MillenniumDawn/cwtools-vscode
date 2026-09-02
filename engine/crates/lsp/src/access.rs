//! The symlink half is the discovery walks' rule (#161), applied to reads so
//!   Not-indexed is the resting state #161 chose; the flip is the price of

use std::path::{Component, Path, PathBuf};

use tower_lsp::lsp_types::Url;

use crate::Backend;

pub(crate) const MAX_URI_READ_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileRead {
    Text(String),
    Missing,
    Refused,
}

impl Backend {
    pub(crate) fn authorized_path(&self, uri: &str) -> Option<PathBuf> {
        let roots = self.state.config.read().authorized_roots.clone();
        authorized_path(uri, &roots)
    }

    pub(crate) fn is_workspace_document(&self, uri: &str) -> bool {
        let roots = self.state.config.read().editable_roots.clone();
        tokio::task::block_in_place(|| workspace_document_path(uri, &roots).is_some())
    }
}

/// Windows verbatim `\\?\` prefix stops them ever matching.
pub(crate) fn authorized_path(uri: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let Some(path) = file_uri_to_path(uri) else {
        tracing::debug!(%uri, "access: not a usable file URI");
        return None;
    };
    let canonical = canonicalize_for_containment(&path)?;
    if !roots.iter().any(|root| canonical.starts_with(root)) {
        tracing::debug!(path = %canonical.display(), "access: outside every authorized root");
        return None;
    }
    // false both for a symlink (the discovery walks' rule, #177) and for
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            tracing::debug!(path = %path.display(), "access: symlink");
            return None;
        }
        if !meta.is_file() {
            tracing::debug!(path = %path.display(), "access: not a regular file");
            return None;
        }
    }
    Some(canonical)
}

pub(crate) fn workspace_document_path(uri: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let path = file_uri_to_path(uri)?;
    let resolved = std::fs::canonicalize(&path)
        .ok()
        .or_else(|| canonicalize_new_path(&path))?;
    if !roots.iter().any(|root| resolved.starts_with(root)) {
        tracing::debug!(path = %resolved.display(), "access: document outside every workspace root");
        return None;
    }
    if let Ok(meta) = path.metadata()
        && !meta.is_file()
    {
        tracing::debug!(path = %path.display(), "access: document is not a regular file");
        return None;
    }
    Some(resolved)
}

pub(crate) fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let url = Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

fn canonicalize_for_containment(path: &Path) -> Option<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Some(canonical),
        Err(_) => {
            let parent = std::fs::canonicalize(path.parent()?).ok()?;
            Some(parent.join(path.file_name()?))
        }
    }
}

/// file exists anywhere on disk (#176). Both sides of the test go through
/// a Windows verbatim `\\?\` prefix does not survive that round trip.
pub(crate) async fn contained_search_path(roots: &[PathBuf], rel: &Path) -> Option<PathBuf> {
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        tracing::debug!(path = %rel.display(), "access: not a plain relative reference");
        return None;
    }
    for root in roots {
        let target = root.join(rel);
        let Ok(canonical) = tokio::fs::canonicalize(&target).await else {
            continue;
        };
        let Ok(canonical_root) = tokio::fs::canonicalize(root).await else {
            continue;
        };
        if canonical.starts_with(&canonical_root) {
            return Some(target);
        }
        tracing::debug!(path = %canonical.display(), "access: reference outside its search root");
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditRefusal {
    NotAFile,
    Symlink,
    NotARegularFile,
    OutsideWorkspace,
    Unresolvable,
}

impl EditRefusal {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::NotAFile => "is not a file on disk",
            Self::Symlink => "is a symbolic link",
            Self::NotARegularFile => "is not a regular file",
            Self::OutsideWorkspace => "is outside the workspace",
            Self::Unresolvable => "cannot be resolved on disk",
        }
    }
}

pub(crate) fn editable_path(uri: &str, roots: &[PathBuf]) -> Result<PathBuf, EditRefusal> {
    let Some(path) = file_uri_to_path(uri) else {
        tracing::debug!(%uri, "access: not a usable file URI for an edit");
        return Err(EditRefusal::NotAFile);
    };
    editable_target(&path, roots)
}

pub(crate) fn editable_target(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, EditRefusal> {
    let resolved = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::debug!(path = %path.display(), "access: edit target is a symlink");
            return Err(EditRefusal::Symlink);
        }
        Ok(meta) if !meta.is_file() => {
            tracing::debug!(path = %path.display(), "access: edit target is not a regular file");
            return Err(EditRefusal::NotARegularFile);
        }
        Ok(_) => std::fs::canonicalize(path).map_err(|_| EditRefusal::Unresolvable)?,
        Err(_) => canonicalize_new_path(path).ok_or(EditRefusal::Unresolvable)?,
    };
    if !roots.iter().any(|root| resolved.starts_with(root)) {
        tracing::debug!(path = %resolved.display(), "access: edit target outside every workspace root");
        return Err(EditRefusal::OutsideWorkspace);
    }
    Ok(resolved)
}

fn canonicalize_new_path(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(base) = std::fs::canonicalize(cursor) {
            return Some(base.join(tail.iter().rev().collect::<PathBuf>()));
        }
        if std::fs::symlink_metadata(cursor).is_ok() {
            return None;
        }
        let Component::Normal(name) = cursor.components().next_back()? else {
            tracing::debug!(path = %path.display(), "access: edit target is not a plain path");
            return None;
        };
        tail.push(name);
        cursor = cursor.parent()?;
    }
}

pub(crate) fn read_authorized_text(uri: &str, roots: &[PathBuf], max_bytes: u64) -> Option<String> {
    match read_authorized(uri, roots, max_bytes) {
        FileRead::Text(text) => Some(text),
        FileRead::Missing | FileRead::Refused => None,
    }
}

pub(crate) fn read_authorized(uri: &str, roots: &[PathBuf], max_bytes: u64) -> FileRead {
    let Some(path) = authorized_path(uri, roots) else {
        return FileRead::Refused;
    };
    read_capped(&path, max_bytes)
}

pub(crate) fn read_capped_text(path: &Path, max_bytes: u64) -> Option<String> {
    match read_capped(path, max_bytes) {
        FileRead::Text(text) => Some(text),
        FileRead::Missing | FileRead::Refused => None,
    }
}

fn read_capped(path: &Path, max_bytes: u64) -> FileRead {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return FileRead::Missing;
    };
    let mut bytes = Vec::new();
    if file
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return FileRead::Missing;
    }
    if bytes.len() as u64 > max_bytes {
        tracing::debug!(path = %path.display(), max_bytes, "access: file over the read cap");
        return FileRead::Refused;
    }
    FileRead::Text(cwtools_file_manager::file_manager::decode_bytes(bytes).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(dirs: [&Path; 1]) -> Vec<PathBuf> {
        dirs.iter()
            .map(|d| std::fs::canonicalize(d).expect("canonical root"))
            .collect()
    }

    fn uri(path: &Path) -> String {
        Url::from_file_path(path)
            .expect("absolute path")
            .to_string()
    }

    #[test]
    fn accepts_a_regular_file_under_a_root() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        assert!(authorized_path(&uri(&file), &roots([tmp.path()])).is_some());
    }

    #[test]
    fn rejects_a_file_outside_every_root() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let other = tempfile::TempDir::new().expect("tmpdir");
        let file = other.path().join("a.txt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        assert_eq!(authorized_path(&uri(&file), &roots([root.path()])), None);
    }

    #[test]
    fn workspace_document_accepts_a_new_nested_file() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let file = root.path().join("new/nested/a.txt");
        assert!(workspace_document_path(&uri(&file), &roots([root.path()])).is_some());
    }

    #[test]
    fn workspace_document_rejects_an_authorized_read_only_root() {
        let workspace = tempfile::TempDir::new().expect("workspace");
        let rules = tempfile::TempDir::new().expect("rules");
        let file = rules.path().join("a.cwt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        assert_eq!(
            workspace_document_path(&uri(&file), &roots([workspace.path()])),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_document_rejects_a_dangling_symlink() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::TempDir::new().expect("workspace");
        let outside = tempfile::TempDir::new().expect("outside");
        let link = workspace.path().join("link");
        symlink(outside.path().join("missing"), &link).unwrap();

        assert_eq!(
            workspace_document_path(&uri(&link), &roots([workspace.path()])),
            None
        );
    }

    /// `to_file_path` only yields a path on Windows when the first segment is a
    #[test]
    fn url_to_file_path_ignores_the_scheme() {
        let converted = Url::parse("http://localhost/C:/Windows")
            .expect("parse")
            .to_file_path();
        assert!(
            converted.is_ok(),
            "to_file_path is scheme-blind; the check in authorized_path is load-bearing"
        );
    }

    #[test]
    fn rejects_a_non_file_scheme() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        let roots = roots([tmp.path()]);
        let http = uri(&file).replacen("file://", "http://localhost", 1);
        assert_eq!(authorized_path(&http, &roots), None);
        assert_eq!(authorized_path("untitled:Untitled-1", &roots), None);
        assert_eq!(
            authorized_path("vscode-vfs://localhost/a.txt", &roots),
            None
        );
    }

    #[test]
    fn rejects_a_uri_that_only_the_raw_fallback_could_convert() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        assert_eq!(
            authorized_path("file://../../etc/passwd", &roots([tmp.path()])),
            None
        );
    }

    #[test]
    fn rejects_a_directory() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let dir = tmp.path().join("sub");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(authorized_path(&uri(&dir), &roots([tmp.path()])), None);
    }

    #[test]
    fn rejects_a_sibling_root_with_a_shared_name_prefix() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path().join("mod");
        let sibling = tmp.path().join("mod-evil");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let file = sibling.join("a.txt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        assert_eq!(authorized_path(&uri(&file), &roots([&root])), None);
    }

    /// of the #177 stance agree on it, since the discovery walks skip the link
    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_pointing_out_of_the_root() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let other = tempfile::TempDir::new().expect("tmpdir");
        let target = other.path().join("secret.txt");
        std::fs::write(&target, "secret\n").unwrap();
        let link = root.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(authorized_path(&uri(&link), &roots([root.path()])), None);
    }

    /// this file, so a URI naming it must not read it either (#177).
    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_that_resolves_inside_the_root() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let target = root.path().join("real.txt");
        std::fs::write(&target, "foo = { }\n").unwrap();
        let link = root.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let roots = roots([root.path()]);
        assert!(
            authorized_path(&uri(&target), &roots).is_some(),
            "the real file behind the link stays readable"
        );
        assert_eq!(authorized_path(&uri(&link), &roots), None);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_character_device() {
        let dev = Path::new("/dev");
        if !dev.is_dir() {
            return;
        }
        assert_eq!(
            authorized_path("file:///dev/zero", &roots([dev])),
            None,
            "a character device is not a readable file"
        );
    }

    #[test]
    fn reads_an_authorized_file() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        assert_eq!(
            read_authorized_text(&uri(&file), &roots([tmp.path()]), MAX_URI_READ_BYTES).as_deref(),
            Some("foo = { }\n")
        );
    }

    #[test]
    fn refuses_a_file_over_the_cap_instead_of_truncating_it() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.txt");
        let roots = roots([tmp.path()]);

        std::fs::write(&file, "0123456789abcdef").unwrap();
        assert_eq!(
            read_authorized_text(&uri(&file), &roots, 16).as_deref(),
            Some("0123456789abcdef"),
            "exactly at the cap is still readable"
        );

        std::fs::write(&file, "0123456789abcdefg").unwrap();
        assert_eq!(
            read_authorized_text(&uri(&file), &roots, 16),
            None,
            "one byte over the cap is refused, not truncated"
        );
    }

    #[test]
    fn an_over_cap_file_is_refused_not_missing() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "0123456789abcdefg").unwrap();
        assert_eq!(
            read_authorized(&uri(&file), &roots([tmp.path()]), 16),
            FileRead::Refused
        );
    }

    /// readable (pre-Jomini mods) rather than dropping them as invalid UTF-8.
    #[test]
    fn decodes_cp1252_like_the_file_manager() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, b"caf\xE9\n").unwrap();
        assert_eq!(
            read_authorized_text(&uri(&file), &roots([tmp.path()]), MAX_URI_READ_BYTES).as_deref(),
            Some("caf\u{E9}\n")
        );
    }

    #[test]
    fn a_deleted_file_under_a_root_reads_as_missing_not_refused() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let gone = tmp.path().join("gone.txt");
        assert_eq!(
            read_authorized(&uri(&gone), &roots([tmp.path()]), MAX_URI_READ_BYTES),
            FileRead::Missing
        );
    }

    #[test]
    fn a_deleted_file_outside_every_root_is_still_refused() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let other = tempfile::TempDir::new().expect("tmpdir");
        let gone = other.path().join("gone.txt");
        assert_eq!(
            read_authorized(&uri(&gone), &roots([root.path()]), MAX_URI_READ_BYTES),
            FileRead::Refused
        );
    }

    #[test]
    fn a_non_file_folder_uri_contributes_no_root() {
        assert_eq!(file_uri_to_path("http://localhost/"), None);
        assert_eq!(file_uri_to_path("untitled:Untitled-1"), None);
        // On Windows the url crate turns the `..` host of a `file:` URI into a
        #[cfg(unix)]
        assert_eq!(file_uri_to_path("file://../../etc"), None);
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        assert_eq!(
            file_uri_to_path(&uri(tmp.path())).as_deref(),
            Some(tmp.path())
        );
    }

    #[test]
    fn refuses_everything_when_no_root_is_configured() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        assert_eq!(authorized_path(&uri(&file), &[]), None);
    }

    #[tokio::test]
    async fn search_path_resolves_under_the_first_root_that_has_the_file() {
        let workspace = tempfile::TempDir::new().expect("workspace");
        let vanilla = tempfile::TempDir::new().expect("vanilla");
        let gfx = vanilla.path().join("gfx");
        std::fs::create_dir(&gfx).unwrap();
        std::fs::write(gfx.join("pic.dds"), "x").unwrap();
        let roots = vec![workspace.path().to_path_buf(), vanilla.path().to_path_buf()];

        let found = contained_search_path(&roots, Path::new("gfx/pic.dds"))
            .await
            .expect("contained");
        assert!(found.starts_with(vanilla.path()));
        assert_eq!(
            contained_search_path(&roots, Path::new("gfx/missing.dds")).await,
            None
        );
    }

    /// (#176). The refusal is syntactic, so it holds on Windows too.
    #[tokio::test]
    async fn search_path_refuses_a_value_that_climbs_out_of_the_root() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path().join("mod");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(tmp.path().join("secret.txt"), "secret\n").unwrap();
        let roots = vec![root.clone()];

        assert_eq!(
            contained_search_path(&roots, Path::new("../secret.txt")).await,
            None
        );
        assert_eq!(
            contained_search_path(&roots, Path::new("gfx/../../secret.txt")).await,
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_path_refuses_a_directory_symlink_pointing_out_of_the_root() {
        let root = tempfile::TempDir::new().expect("root");
        let other = tempfile::TempDir::new().expect("other");
        std::fs::write(other.path().join("secret.txt"), "secret\n").unwrap();
        std::os::unix::fs::symlink(other.path(), root.path().join("gfx")).unwrap();
        assert_eq!(
            contained_search_path(&[root.path().to_path_buf()], Path::new("gfx/secret.txt")).await,
            None
        );
    }

    #[tokio::test]
    async fn search_path_refuses_everything_when_no_root_is_configured() {
        assert_eq!(
            contained_search_path(&[], Path::new("gfx/pic.dds")).await,
            None
        );
    }

    #[test]
    fn accepts_a_regular_file_under_an_edit_root() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("localisation/x_l_english.yml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "l_english:\n").unwrap();
        assert!(editable_path(&uri(&file), &roots([tmp.path()])).is_ok());
    }

    #[test]
    fn rejects_a_sibling_edit_root_with_a_shared_name_prefix() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path().join("mod");
        let sibling = tmp.path().join("mod-vanilla");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let file = sibling.join("x_l_english.yml");
        std::fs::write(&file, "l_english:\n").unwrap();
        assert_eq!(
            editable_path(&uri(&file), &roots([&root])),
            Err(EditRefusal::OutsideWorkspace)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_leaf_symlink_pointing_out_of_the_root() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let other = tempfile::TempDir::new().expect("tmpdir");
        let target = other.path().join("secret.txt");
        std::fs::write(&target, "secret\n").unwrap();
        let link = root.path().join("x_l_english.yml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            editable_path(&uri(&link), &roots([root.path()])),
            Err(EditRefusal::Symlink)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_leaf_symlink_even_when_its_target_is_inside_the_root() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let target = root.path().join("real_l_english.yml");
        std::fs::write(&target, "l_english:\n").unwrap();
        let link = root.path().join("link_l_english.yml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            editable_path(&uri(&link), &roots([root.path()])),
            Err(EditRefusal::Symlink)
        );
        assert!(
            editable_path(&uri(&target), &roots([root.path()])).is_ok(),
            "the real file behind the link is still editable"
        );
    }

    #[test]
    fn accepts_a_new_file_whose_parent_directory_does_not_exist_yet() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let new = tmp
            .path()
            .join("localisation/cwtools_generated_l_english.yml");
        let resolved = editable_path(&uri(&new), &roots([tmp.path()])).expect("contained");
        assert!(resolved.ends_with("localisation/cwtools_generated_l_english.yml"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_new_file_under_a_directory_symlink_pointing_out_of_the_root() {
        let root = tempfile::TempDir::new().expect("tmpdir");
        let other = tempfile::TempDir::new().expect("tmpdir");
        std::os::unix::fs::symlink(other.path(), root.path().join("localisation")).unwrap();
        let new = root
            .path()
            .join("localisation/cwtools_generated_l_english.yml");
        assert_eq!(
            editable_path(&uri(&new), &roots([root.path()])),
            Err(EditRefusal::OutsideWorkspace)
        );
    }

    /// `realpath` refuses the `gone/..` pair outright (`Unresolvable`); Windows
    #[test]
    fn rejects_a_new_path_that_climbs_out_with_dot_dot() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path().join("mod");
        std::fs::create_dir(&root).unwrap();
        let climbing = root.join("gone/../../escaped.yml");
        assert!(editable_target(&climbing, &roots([&root])).is_err());
    }

    #[test]
    fn rejects_a_directory_and_a_non_file_scheme() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let dir = tmp.path().join("localisation");
        std::fs::create_dir(&dir).unwrap();
        let roots = roots([tmp.path()]);
        assert_eq!(
            editable_path(&uri(&dir), &roots),
            Err(EditRefusal::NotARegularFile)
        );
        let file = tmp.path().join("a.yml");
        std::fs::write(&file, "l_english:\n").unwrap();
        let http = uri(&file).replacen("file://", "http://localhost", 1);
        assert_eq!(editable_path(&http, &roots), Err(EditRefusal::NotAFile));
        assert_eq!(
            editable_path("untitled:Untitled-1", &roots),
            Err(EditRefusal::NotAFile)
        );
    }

    #[test]
    fn refuses_every_edit_when_no_root_is_configured() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("a.yml");
        std::fs::write(&file, "l_english:\n").unwrap();
        assert_eq!(
            editable_path(&uri(&file), &[]),
            Err(EditRefusal::OutsideWorkspace)
        );
    }

    #[test]
    fn every_refusal_names_its_cause() {
        for refusal in [
            EditRefusal::NotAFile,
            EditRefusal::Symlink,
            EditRefusal::NotARegularFile,
            EditRefusal::OutsideWorkspace,
            EditRefusal::Unresolvable,
        ] {
            assert!(refusal.reason().starts_with("is ") || refusal.reason().starts_with("cannot "));
        }
    }
}
