//! The only place a client-supplied URI becomes an opened file.
//!
//! Every per-file handler takes a URI straight from the client, and the LSP has
//! no say in what a client sends. Before this module the URI was converted with
//! [`crate::paths::uri_to_path_str`] and handed to an unbounded read, which let
//! a request name `file:///dev/zero`, a device or a file anywhere on the disk.
//! `uri_to_path_str` stays as-is for the derivations that only ever *display* or
//! *label* a path (logical paths, loc-dir tests, graph node ids); reads go
//! through [`read_authorized_text`] and nothing else.
//!
//! A URI is authorized when it is a `file:` URI, names a regular file that is
//! not a symlink, and canonicalizes inside one of the roots the server was
//! configured with (the workspace folders, the base-game install, the rules
//! dir). Anything else is refused quietly: requests answer `None`,
//! notifications no-op, no index moves.
//!
//! The symlink half is the discovery walks' rule (#161), applied to reads so
//! the two agree: nothing a scan refuses to index can be pulled in later by a
//! URI naming it. Without that, a `didChangeWatchedFiles` event on a symlinked
//! file read, parsed, indexed and published diagnostics for a file the scan had
//! deliberately skipped. Two consequences are deliberate rather than overlooked:
//!
//! - A symlinked *ancestor* directory still resolves. Containment refuses one
//!   pointing out of a root; one resolving back inside lands on the real file
//!   the scan already indexed under its canonical path. Catching those needs a
//!   per-component `lstat` on every read (hundreds per rename) for a case that
//!   reaches the right bytes anyway.
//! - Open buffers are admitted on containment alone
//!   ([`workspace_document_path`]), so a symlinked file can still be edited with
//!   live diagnostics. Its exports enter the index when the buffer opens and
//!   leave when it closes, because this boundary then refuses the disk read.
//!   Not-indexed is the resting state #161 chose; the flip is the price of
//!   editing such a file at all.
//!
//! Edits get their own, stricter boundary in [`editable_path`]: a generated
//! `WorkspaceEdit` may only name a path inside a *workspace* root, and may not
//! name a symlink at all. The base game and the rules dir are readable but
//! never writable, so the two boundaries take different root lists
//! ([`crate::Config::authorized_roots`] and [`crate::Config::editable_roots`]).
//!
//! A path a mod *writes* rather than a client sends gets a third and smaller
//! boundary in [`contained_search_path`]: the document-link and goto probes
//! join a `filepath[..]` leaf value onto the search roots and stat the result,
//! which without containment answers whether a file exists anywhere on disk.

use std::path::{Component, Path, PathBuf};

use tower_lsp::lsp_types::Url;

use crate::Backend;

/// Liveness ceiling on a single URI-driven read, not a resource policy: the
/// largest known legitimate input is a ~20 MB vanilla script.
pub(crate) const MAX_URI_READ_BYTES: u64 = 64 * 1024 * 1024;

/// The outcome of a URI-driven read.
///
/// Most callers only want the text and use [`read_authorized_text`]. `did_close`
/// needs the other two apart: it clears a file's index entry when the file has
/// gone from disk, and must not do that merely because the boundary said no.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileRead {
    Text(String),
    /// Authorized, but there is nothing to read — deleted, or unreadable.
    Missing,
    /// Outside the boundary: not a `file:` URI, out of every root, not a
    /// regular file, or over the cap.
    Refused,
}

impl Backend {
    /// The canonical path `uri` names, when the client is allowed to name it.
    /// `None` (and no state change) for anything else — see the module docs.
    pub(crate) fn authorized_path(&self, uri: &str) -> Option<PathBuf> {
        let roots = self.state.config.read().authorized_roots.clone();
        authorized_path(uri, &roots)
    }

    pub(crate) fn is_workspace_document(&self, uri: &str) -> bool {
        let roots = self.state.config.read().editable_roots.clone();
        tokio::task::block_in_place(|| workspace_document_path(uri, &roots).is_some())
    }
}

/// The canonical path `uri` names, when it is a `file:` URI naming a regular,
/// non-symlink file inside `roots`. `roots` must already be canonical
/// ([`crate::Config::refresh_roots`] keeps them that way) — both
/// sides of the containment test have to come from `canonicalize` or the
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
    // Only when it exists: a path that has just been deleted is authorized but
    // absent, which `did_close` has to tell apart from refused. Taken on the
    // unresolved path so the leaf's own type is what gets tested. `is_file` is
    // false both for a symlink (the discovery walks' rule, #177) and for
    // `/dev/zero`, whose length-0 report a size check alone would wave through;
    // the explicit symlink branch is only there to name the cause in the log.
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

/// Resolve a client-owned document URI inside a configured workspace folder.
/// Unlike [`authorized_path`], this excludes the base-game and rules roots and
/// accepts a not-yet-created file whose deepest existing ancestor is in the
/// workspace.
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

/// The path a `file:` URI names, unresolved. `None` for any other scheme.
///
/// The strict counterpart to [`crate::paths::uri_to_path_str`], which anything
/// deriving a *root* must use too: `Url::to_file_path` ignores the scheme (with
/// an empty or `localhost` host it turns `http://localhost/etc/passwd` into
/// `/etc/passwd`), and the lax converter's raw-string fallback turns
/// `file://../../etc/passwd` into a relative path resolved against the CWD. A
/// `rootUri` of `http://localhost/` would otherwise authorize the filesystem.
pub(crate) fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let url = Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

/// Canonical form of `path` for the containment test. Resolving the whole path
/// also resolves a symlinked *file*, so a link inside a root pointing out of it
/// is caught. `canonicalize` needs the file to exist, though, and a deleted file
/// still needs an answer — so fall back to resolving the directory chain and
/// re-attaching the name.
fn canonicalize_for_containment(path: &Path) -> Option<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Some(canonical),
        Err(_) => {
            let parent = std::fs::canonicalize(path.parent()?).ok()?;
            Some(parent.join(path.file_name()?))
        }
    }
}

/// The first of `roots` under which the relative reference `rel` names an
/// existing file, or `None` when it names none of them.
///
/// A `filepath[..]` leaf value is mod content rather than a client URI, but the
/// document-link and goto probes join it onto the search roots and stat the
/// result — so without a containment test a `../`-laden value reports whether a
/// file exists anywhere on disk (#176). Both sides of the test go through
/// `canonicalize` as in [`authorized_path`], which resolves a symlinked target
/// too; `roots` arrive unresolved here, unlike the authorized ones. What comes
/// back is the plain join, since that is what the client is handed as a URI and
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
        // The resolve doubles as the existence probe, so a reference naming
        // nothing costs the one call the bare stat used to.
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

/// Why a path may not be the target of a server-generated edit. Carried so a
/// refusal the user sees (the rename cancellation) can name the actual cause
/// instead of guessing at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditRefusal {
    /// Not a `file:` URI, so it names nothing on disk to contain.
    NotAFile,
    /// The leaf is a symlink. Refused rather than followed — see
    /// [`editable_target`].
    Symlink,
    /// Exists, but is a directory, a device, or another non-regular file.
    NotARegularFile,
    /// Resolves outside every workspace root.
    OutsideWorkspace,
    /// Doesn't resolve at all: a `..` that would climb past the canonical
    /// ancestor, or a parent directory that can't be read.
    Unresolvable,
}

impl EditRefusal {
    /// The cause as a predicate, for a message of the form `'<uri>' <reason>`.
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

/// The canonical path `uri` names, when a server-generated edit may write to
/// it, else why it may not.
///
/// The write-side counterpart of [`authorized_path`], and stricter in two ways
/// because an edit changes what a read only observes: `roots` here is
/// [`crate::Config::editable_roots`] — the workspace folders alone, never the
/// base-game install or the rules dir — and a symlink at the leaf is refused
/// outright rather than followed.
///
/// The leaf rule is the tighter half. Resolving a link would already catch one
/// aiming out of the workspace, but the leaf is the component the workspace
/// author can re-point between this check and the client's write, so a link
/// there is refused even when it currently resolves back inside a root. That
/// narrows the window to an active swap; it cannot close it, because the client
/// performs the write long after the server answers.
pub(crate) fn editable_path(uri: &str, roots: &[PathBuf]) -> Result<PathBuf, EditRefusal> {
    let Some(path) = file_uri_to_path(uri) else {
        tracing::debug!(%uri, "access: not a usable file URI for an edit");
        return Err(EditRefusal::NotAFile);
    };
    editable_target(&path, roots)
}

/// [`editable_path`] for a path the server derived itself rather than one that
/// arrived as a URI — the loc-file candidates the create-loc-key action picks
/// between.
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
        // Resolves any symlink among the ancestors; the leaf is already known
        // not to be one.
        Ok(_) => std::fs::canonicalize(path).map_err(|_| EditRefusal::Unresolvable)?,
        // No metadata at all. Usually the file simply isn't there yet (the
        // create-loc-key `NewFile` tier), but an unreadable or non-directory
        // parent lands here too — `canonicalize_new_path` refuses those rather
        // than assuming the path is merely new.
        Err(_) => canonicalize_new_path(path).ok_or(EditRefusal::Unresolvable)?,
    };
    if !roots.iter().any(|root| resolved.starts_with(root)) {
        tracing::debug!(path = %resolved.display(), "access: edit target outside every workspace root");
        return Err(EditRefusal::OutsideWorkspace);
    }
    Ok(resolved)
}

/// Canonical form of a path that does not exist yet, for an edit that creates
/// its target: resolve the deepest ancestor that *does* exist and re-attach the
/// missing tail.
///
/// The read path's [`canonicalize_for_containment`] falls back one level, which
/// is enough for a file that was just deleted but not for the create-loc-key
/// `NewFile` tier — it targets `<workspace>/localisation/…` in a workspace that
/// may have no `localisation` directory at all. Every missing component has to
/// be a plain name: a `..` can't be resolved against the canonical ancestor, so
/// it is refused rather than normalized.
fn canonicalize_new_path(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(base) = std::fs::canonicalize(cursor) {
            return Some(base.join(tail.iter().rev().collect::<PathBuf>()));
        }
        // Do not reinterpret a dangling symlink as a new plain component.
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

/// [`authorized_path`] followed by a read bounded at `max_bytes`, for the
/// callers that only need the text.
pub(crate) fn read_authorized_text(uri: &str, roots: &[PathBuf], max_bytes: u64) -> Option<String> {
    match read_authorized(uri, roots, max_bytes) {
        FileRead::Text(text) => Some(text),
        FileRead::Missing | FileRead::Refused => None,
    }
}

/// [`authorized_path`] followed by a read bounded at `max_bytes`, reporting why
/// there is no text when there isn't any. See [`FileRead`].
pub(crate) fn read_authorized(uri: &str, roots: &[PathBuf], max_bytes: u64) -> FileRead {
    let Some(path) = authorized_path(uri, roots) else {
        return FileRead::Refused;
    };
    read_capped(&path, max_bytes)
}

/// Bounded read of a path that has already cleared [`authorized_path`], for the
/// watched-file batch — it needs the canonical path in hand anyway, for its
/// stat-gate.
pub(crate) fn read_capped_text(path: &Path, max_bytes: u64) -> Option<String> {
    match read_capped(path, max_bytes) {
        FileRead::Text(text) => Some(text),
        FileRead::Missing | FileRead::Refused => None,
    }
}

/// Read `path` as text, refusing it outright once it passes `max_bytes` — a
/// truncated script would parse into garbage, so an over-cap file is no file.
fn read_capped(path: &Path, max_bytes: u64) -> FileRead {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return FileRead::Missing;
    };
    let mut bytes = Vec::new();
    // `take` bounds the allocation as well as the read, so a file that grows
    // under us or misreports its length still can't outrun the cap.
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

    /// Roots reach [`authorized_path`] already canonicalized; a raw tempdir path
    /// is not (macOS hands out `/var/folders/…` for `/private/var/folders/…`).
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

    /// The `url` crate's `to_file_path` ignores the scheme, so a non-`file` URI
    /// with an empty or `localhost` host converts to a perfectly good absolute
    /// path. This is the behaviour the explicit scheme check exists to stop;
    /// asserting it here means a `url` upgrade can't quietly move the goalposts.
    ///
    /// The fixture uses a drive-letter path rather than `/etc/passwd` because
    /// `to_file_path` only yields a path on Windows when the first segment is a
    /// drive letter; either host resolves the same scheme-blind conversion.
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
        // Same file, reachable path, wrong scheme.
        let http = uri(&file).replacen("file://", "http://localhost", 1);
        assert_eq!(authorized_path(&http, &roots), None);
        assert_eq!(authorized_path("untitled:Untitled-1", &roots), None);
        assert_eq!(
            authorized_path("vscode-vfs://localhost/a.txt", &roots),
            None
        );
    }

    /// `file://../../etc/passwd` parses with `..` as the *host*, which
    /// `to_file_path` rejects — the old raw-string fallback then produced a
    /// relative path read against the server's CWD.
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

    /// Containment is component-wise: a string prefix would let a sibling
    /// directory whose name merely starts with the root's name through.
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

    /// Containment alone already refuses this one; the pin is that both sides
    /// of the #177 stance agree on it, since the discovery walks skip the link
    /// too.
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

    /// The half containment can't reach: the link resolves back inside the root,
    /// so only the leaf's own type refuses it. The discovery walks never index
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
        // Rooted at `/dev` so the regular-file gate is what does the refusing,
        // not containment. `/dev/zero` reads forever; that was the reported bug.
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

    /// The cap is a parameter so this doesn't have to write 64 MiB to prove it.
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

    /// Over-cap must be `Refused`, never `Missing`: `did_close` maps `Missing`
    /// to "gone from disk" and clears the file's index entry, so flipping this
    /// would start wiping the index for large in-workspace files on close.
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

    /// The boundary owns the read, so it also owns keeping cp1252 script files
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

    /// A deleted file inside a root is `Missing`, not `Refused`: `did_close`
    /// drops a file's index entry when it has gone from disk, and must not stop
    /// doing that just because the path no longer resolves.
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

    /// Roots go through the strict conversion too. `uri_to_path_str` would turn
    /// `http://localhost/` into the path `/` — as a root, that authorizes the
    /// whole filesystem.
    #[test]
    fn a_non_file_folder_uri_contributes_no_root() {
        assert_eq!(file_uri_to_path("http://localhost/"), None);
        assert_eq!(file_uri_to_path("untitled:Untitled-1"), None);
        // On Windows the url crate turns the `..` host of a `file:` URI into a
        // UNC path (`\\..\etc`); `refresh_roots` drops it because a bogus
        // host can't canonicalize, and the authorization boundary refuses it
        // (`rejects_a_uri_that_only_the_raw_fallback_could_convert`). The
        // strict converter is `None` only where hosts can't be UNC servers.
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

    // ── The search-root boundary ──────────────────────────────────────────

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

    /// The reported hole: the join was stated with no containment test, so a
    /// `../`-laden leaf value answered whether a file exists outside the roots
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

    /// The half the syntactic refusal can't reach, and the reason containment
    /// is canonical: every component is a plain name, but one of them is a link
    /// out of the root.
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

    // ── The edit boundary ─────────────────────────────────────────────────

    #[test]
    fn accepts_a_regular_file_under_an_edit_root() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("localisation/x_l_english.yml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "l_english:\n").unwrap();
        assert!(editable_path(&uri(&file), &roots([tmp.path()])).is_ok());
    }

    /// The reported bug: workspace `/ws/mod` with the base game at
    /// `/ws/mod-vanilla` — a string prefix says the vanilla file is in the
    /// workspace, and the create-loc-key action then offers to edit the
    /// base-game install.
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

    /// Stricter than the read side on purpose: even a link that resolves back
    /// inside the root is refused, because the leaf is the one component the
    /// workspace author can re-point between this check and the client's write.
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

    /// The create-loc-key `NewFile` tier: neither the file nor its directory
    /// exists yet, so `canonicalize` can't resolve either.
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

    /// A `..` in the not-yet-existing tail can't be resolved against the
    /// canonical ancestor, so it is refused rather than normalized. Linux
    /// `realpath` refuses the `gone/..` pair outright (`Unresolvable`); Windows
    /// `canonicalize` collapses the pair instead and reports the climb past the
    /// root (`OutsideWorkspace`). Either way the edit is refused, never written.
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

    /// Every cause reads as `'<uri>' <reason>` in the rename refusal.
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
