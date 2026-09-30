//! The symlink half is the discovery walks' rule (#161), applied to reads so
//!   Not-indexed is the resting state #161 chose; the flip is the price of

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use tower_lsp::lsp_types::Url;

use crate::Backend;

pub(crate) const MAX_URI_READ_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_NAVIGATION_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;

/// A per-request memory allowance for retained navigation text snapshots.
#[derive(Debug)]
pub(crate) struct ReadBudget {
    limit: usize,
    reserved: AtomicUsize,
}

impl ReadBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            reserved: AtomicUsize::new(0),
        }
    }

    pub(crate) fn reserve_retained(&self, bytes: usize) -> bool {
        let Some(reservation) = self.reserve(bytes) else {
            return false;
        };
        reservation.retain(bytes);
        true
    }

    fn reserve(&self, bytes: usize) -> Option<ReadReservation<'_>> {
        let mut current = self.reserved.load(Ordering::Relaxed);
        loop {
            let next = current.checked_add(bytes)?;
            if next > self.limit {
                return None;
            }
            match self.reserved.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ReadReservation {
                        budget: self,
                        bytes,
                        active: true,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

struct ReadReservation<'budget> {
    budget: &'budget ReadBudget,
    bytes: usize,
    active: bool,
}

impl ReadReservation<'_> {
    fn retain(mut self, bytes: usize) {
        debug_assert!(bytes <= self.bytes);
        self.budget
            .reserved
            .fetch_sub(self.bytes.saturating_sub(bytes), Ordering::AcqRel);
        self.active = false;
    }
}

impl Drop for ReadReservation<'_> {
    fn drop(&mut self) {
        if self.active {
            self.budget.reserved.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadBudgetExceeded;

/// Reads one authorized file after reserving its peak snapshot memory.
///
/// The reservation covers the bounded input buffer and up to four bytes of
/// decoded `String` capacity per input byte (the `decode_bytes` fallback
/// collects Windows-1252 characters). On success it is reduced to the
/// retained string capacity; failed and changed files release the reservation.
pub(crate) fn read_authorized_text_with_budget(
    uri: &str,
    roots: &[PathBuf],
    max_file_bytes: u64,
    budget: &ReadBudget,
) -> Result<Option<String>, ReadBudgetExceeded> {
    let Some(path) = authorized_path(uri, roots) else {
        return Ok(None);
    };
    #[cfg(test)]
    RECORDED_READS.lock().push(path.clone());
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(None);
    };
    let Ok(metadata) = file.metadata() else {
        return Ok(None);
    };
    let source_bytes = metadata.len();
    if !metadata.is_file() || source_bytes > max_file_bytes {
        return Ok(None);
    }
    read_text_with_budget(file, source_bytes, budget)
}

fn read_text_with_budget(
    reader: impl std::io::Read,
    source_bytes: u64,
    budget: &ReadBudget,
) -> Result<Option<String>, ReadBudgetExceeded> {
    use std::io::Read as _;

    let Some(read_limit) = source_bytes.checked_add(1) else {
        return Ok(None);
    };
    let Ok(read_capacity) = usize::try_from(read_limit) else {
        return Ok(None);
    };
    let Some(peak_bytes) = read_capacity.checked_mul(5) else {
        return Err(ReadBudgetExceeded);
    };
    let Some(reservation) = budget.reserve(peak_bytes) else {
        return Err(ReadBudgetExceeded);
    };

    let mut bytes = Vec::with_capacity(read_capacity);
    let Ok(read) = reader.take(read_limit).read_to_end(&mut bytes) else {
        return Ok(None);
    };
    if read as u64 > source_bytes {
        return Ok(None);
    }
    let (text, _) = cwtools_file_manager::file_manager::decode_bytes(bytes);
    let retained_bytes = text.capacity();
    debug_assert!(retained_bytes <= peak_bytes);
    reservation.retain(retained_bytes);
    Ok(Some(text))
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

/// Every path this module has actually opened, for tests that assert a code
/// path performs no disk reads (#472). A recorded list rather than a counter:
/// `cargo test` runs tests in parallel threads inside one binary, so a caller
/// filters by its own fixture root and is unaffected by other tests reading
/// their own files.
#[cfg(test)]
static RECORDED_READS: parking_lot::Mutex<Vec<PathBuf>> = parking_lot::Mutex::new(Vec::new());

/// How many reads this module has performed under `root`.
///
/// Recorded paths arrive in whatever form the caller had: `read_authorized`
/// canonicalizes first, which on Windows yields a verbatim `\\?\` prefix that
/// a plain fixture path never matches, while `read_capped_text` passes its
/// path through untouched. Both forms of the root are tried so the filter does
/// not silently match nothing.
#[cfg(test)]
pub(crate) fn recorded_reads_under(root: &Path) -> usize {
    let canonical = canonicalize_for_containment(root);
    RECORDED_READS
        .lock()
        .iter()
        .filter(|path| {
            path.starts_with(root)
                || canonical
                    .as_deref()
                    .is_some_and(|canonical| path.starts_with(canonical))
        })
        .count()
}

fn read_capped(path: &Path, max_bytes: u64) -> FileRead {
    use std::io::Read as _;
    #[cfg(test)]
    RECORDED_READS.lock().push(path.to_path_buf());
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

    struct FailsAfterByte(bool);

    impl std::io::Read for FailsAfterByte {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::Error::other("injected read failure"));
            }
            let Some(first) = buffer.first_mut() else {
                return Ok(0);
            };
            *first = b'x';
            self.0 = true;
            Ok(1)
        }
    }

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
    fn read_budget_reservations_are_cumulative_under_concurrency() {
        use std::sync::{Arc, Barrier};

        let workers = 8;
        let budget = Arc::new(ReadBudget::new(3));
        let barrier = Arc::new(Barrier::new(workers));
        let reserved = std::thread::scope(|scope| {
            let handles = (0..workers)
                .map(|_| {
                    let budget = Arc::clone(&budget);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        budget.reserve_retained(1)
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("reservation thread completes"))
                .filter(|reserved| *reserved)
                .count()
        });
        assert_eq!(reserved, 3);
    }

    #[test]
    fn budgeted_reads_reserve_decode_expansion_before_allocating() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("cp1252.txt");
        std::fs::write(&file, [0x81; 4]).unwrap();
        let roots = roots([tmp.path()]);
        let uri = uri(&file);

        let budget = ReadBudget::new(25);
        let text = read_authorized_text_with_budget(&uri, &roots, MAX_URI_READ_BYTES, &budget)
            .expect("budget fits")
            .expect("authorized file is readable");
        assert_eq!(text, "\u{FFFD}".repeat(4));

        let budget = ReadBudget::new(24);
        assert_eq!(
            read_authorized_text_with_budget(&uri, &roots, MAX_URI_READ_BYTES, &budget,),
            Err(ReadBudgetExceeded),
            "the five-byte read allowance includes decoding expansion"
        );
    }

    #[test]
    fn missing_and_over_cap_budgeted_reads_release_their_reservations() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let gone = tmp.path().join("gone.txt");
        let large = tmp.path().join("large.txt");
        std::fs::write(&large, "ab").unwrap();
        let roots = roots([tmp.path()]);
        let budget = ReadBudget::new(5);

        assert_eq!(
            read_authorized_text_with_budget(&uri(&gone), &roots, MAX_URI_READ_BYTES, &budget,),
            Ok(None)
        );
        assert_eq!(
            read_authorized_text_with_budget(&uri(&large), &roots, 1, &budget),
            Ok(None),
            "an over-cap file is refused before reserving or reading it"
        );
        assert!(
            budget.reserve_retained(5),
            "neither refusal consumed budget"
        );
    }

    #[test]
    fn post_reservation_read_failure_releases_the_full_allowance() {
        let budget = ReadBudget::new(25);
        assert_eq!(
            read_text_with_budget(FailsAfterByte(false), 4, &budget),
            Ok(None),
            "the injected read error occurs after reservation and one byte"
        );
        assert!(budget.reserve_retained(25));
    }

    #[test]
    fn file_growth_after_metadata_releases_the_full_allowance() {
        let budget = ReadBudget::new(10);
        assert_eq!(
            read_text_with_budget(std::io::Cursor::new(b"ab"), 1, &budget),
            Ok(None),
            "the reader returns one byte beyond its metadata size"
        );
        assert!(budget.reserve_retained(10));
    }

    #[test]
    fn read_budget_handles_empty_input_at_exact_boundary() {
        assert_eq!(
            read_text_with_budget(std::io::Cursor::new(b""), 0, &ReadBudget::new(5)),
            Ok(Some(String::new()))
        );
        assert_eq!(
            read_text_with_budget(std::io::Cursor::new(b""), 0, &ReadBudget::new(4)),
            Err(ReadBudgetExceeded)
        );
    }

    #[test]
    fn read_budget_enforces_exact_closed_file_boundary() {
        let exact = b"0123456789";
        assert_eq!(
            read_text_with_budget(std::io::Cursor::new(exact), 10, &ReadBudget::new(54)),
            Err(ReadBudgetExceeded)
        );
        assert_eq!(
            read_text_with_budget(std::io::Cursor::new(exact), 10, &ReadBudget::new(55)),
            Ok(Some(String::from_utf8_lossy(exact).into_owned()))
        );
    }

    #[test]
    fn read_budget_accepts_zero_byte_reservation_at_zero_limit() {
        let budget = ReadBudget::new(0);
        assert!(budget.reserve_retained(0));
        assert!(!budget.reserve_retained(1));
    }

    #[test]
    fn read_budget_rejects_arithmetic_overflow() {
        let budget = ReadBudget::new(usize::MAX);
        assert!(budget.reserve_retained(usize::MAX));
        assert!(!budget.reserve_retained(1));
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
