//! Bounds on cache inputs (#162).
//!
//! A cache path is chosen by a CLI flag or an LSP client, so neither the read
//! nor the zstd decode behind it may run unbounded. Every rejection here has to
//! surface as a `CacheError`, which is what `cwtools_cache::workspace::load`
//! collapses to a re-parse.

use cwtools_cache::io::{
    self, MAX_ARCHIVE_DECODED_BYTES, MAX_ARCHIVE_FILE_BYTES, MAX_ERRORS_FILE_BYTES, decode_capped,
    read_capped,
};
use std::fs::File;
use std::io::Write;
use std::path::Path;

const MAGIC: [u8; 4] = *b"CWB\x00";
const FORMAT_VERSION: u8 = 4;
const ERRORS_MAGIC: [u8; 4] = *b"CWE\x00";
const ERRORS_FORMAT_VERSION: u8 = 1;

/// A complete frame of `len` zero bytes whose header does not declare how much
/// it decompresses to. Both cache writers stream, so this is the shape of every
/// frame the caches themselves hold, and what the byte-by-byte bound has to
/// catch with no help from the header.
fn undeclared_frame(len: usize) -> Vec<u8> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
    encoder.write_all(&vec![0u8; len]).unwrap();
    let frame = encoder.finish().unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&frame).unwrap(),
        None,
        "fixture must not declare its size, or it never reaches the streaming bound"
    );
    frame
}

/// A complete frame of `len` zero bytes that does declare its size, the shape a
/// one-shot compressor such as the `zstd` command line produces.
fn declared_frame(len: usize) -> Vec<u8> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
    encoder.set_pledged_src_size(Some(len as u64)).unwrap();
    encoder.write_all(&vec![0u8; len]).unwrap();
    let frame = encoder.finish().unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&frame).unwrap(),
        Some(len as u64),
        "fixture must declare its size, or it never reaches the header check"
    );
    frame
}

/// A frame whose header declares `declared` bytes while carrying almost none of
/// them. zstd will not close a frame that lies about its size, so this is the
/// prefix the encoder had already emitted, which is all the header check reads.
fn overstated_frame(declared: u64) -> Vec<u8> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
    encoder.set_pledged_src_size(Some(declared)).unwrap();
    encoder.write_all(b"nowhere near that many bytes").unwrap();
    encoder.flush().unwrap();
    let frame = encoder.get_ref().clone();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&frame).unwrap(),
        Some(declared),
        "fixture must declare its size, or it never reaches the header check"
    );
    frame
}

/// A sink that takes one byte per `write`, the way a pipe or a short write can.
/// The cap has to count what was taken, not what was offered.
struct OneByteAtATime(Vec<u8>);

impl Write for OneByteAtATime {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match buf.first() {
            Some(byte) => {
                self.0.push(*byte);
                Ok(1)
            }
            None => Ok(0),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Grow `path` to `len` without writing the bytes, so a test for an over-cap
/// file costs no disk.
fn extend_sparse(path: &Path, header: &[u8], len: u64) {
    let mut file = File::create(path).unwrap();
    file.write_all(header).unwrap();
    file.set_len(len).unwrap();
}

#[cfg(unix)]
#[test]
fn a_character_device_is_refused() {
    // `/dev/zero` reports length 0, so a size check alone waves it through and
    // then reads to EOF. Only the regular-file gate stops it.
    let err = read_capped(Path::new("/dev/zero"), 1024).unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "{err}");

    let err = io::with_archived_file(Path::new("/dev/zero"), |_| ()).unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "{err}");
}

#[test]
fn a_directory_is_refused() {
    // Windows refuses the open outright and Unix gets as far as the metadata,
    // so only the rejection itself is portable.
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_capped(tmp.path(), 1024).is_err());
    assert!(io::with_archived_file(tmp.path(), |_| ()).is_err());
}

#[cfg(unix)]
#[test]
fn a_symlink_is_followed_to_whatever_it_points_at() {
    // Deliberately unlike `vanilla_cache::is_cache_file`, which refuses a
    // symlink because it guards a delete. This guards a read, and a cache kept
    // outside the cache dir and linked in is a reasonable thing to do, so the
    // gate has to judge the target rather than the link.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("real");
    std::fs::write(&target, b"cache bytes").unwrap();
    let to_file = tmp.path().join("to-file");
    let to_device = tmp.path().join("to-device");
    std::os::unix::fs::symlink(&target, &to_file).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &to_device).unwrap();

    assert_eq!(read_capped(&to_file, 1024).unwrap(), b"cache bytes");
    let err = read_capped(&to_device, 1024).unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_file_whose_length_reads_as_zero_is_still_capped() {
    // The cap is judged on the bytes that arrive, never on the reported length,
    // which is what holds when a file grows between the stat and the read. A
    // procfs entry is the deterministic version of that: a regular file that
    // reports zero bytes and then hands over real content.
    let status = Path::new("/proc/self/status");
    assert_eq!(std::fs::metadata(status).unwrap().len(), 0);

    let err = read_capped(status, 16).unwrap_err();
    assert!(err.to_string().contains("cache read cap"), "{err}");
    assert!(read_capped(status, 1024 * 1024).unwrap().len() > 16);
}

#[test]
fn read_capped_accepts_a_file_exactly_at_the_cap() {
    // The cap is inclusive. Nothing else here would notice `>` turning into
    // `>=` and every cache at the boundary becoming a miss.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("at-cap");
    std::fs::write(&path, vec![7u8; 1024]).unwrap();

    assert_eq!(read_capped(&path, 1024).unwrap().len(), 1024);
    assert!(read_capped(&path, 1023).is_err());
}

#[test]
fn an_archive_over_the_read_cap_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("huge.cwb");
    let mut header = MAGIC.to_vec();
    header.push(FORMAT_VERSION);
    extend_sparse(&path, &header, MAX_ARCHIVE_FILE_BYTES + 1);

    let err = io::with_archived_file(&path, |_| ()).unwrap_err();
    assert!(err.to_string().contains("cache read cap"), "{err}");
}

#[test]
fn an_archive_declaring_an_over_cap_body_is_refused() {
    // The one thing the helper's own tests cannot show: that the `.cwb` loader
    // hands `decode_capped` its cap and not `u64::MAX`. A body declaring one
    // byte past it must come back as the cap error, not as the truncated-frame
    // error a pass-through would produce.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bomb.cwb");
    let mut body = MAGIC.to_vec();
    body.push(FORMAT_VERSION);
    body.extend_from_slice(&overstated_frame(MAX_ARCHIVE_DECODED_BYTES + 1));
    std::fs::write(&path, &body).unwrap();

    // `CacheError::Compression` keeps the reason in its source, so read the
    // whole thing: without the cap this is a truncated-frame error instead.
    let err = io::with_archived_file(&path, |_| ()).unwrap_err();
    assert!(
        format!("{err:?}").contains("decompresses past"),
        "got {err:?}"
    );
}

#[test]
fn an_error_sidecar_over_the_read_cap_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("huge.cwe");
    let mut header = ERRORS_MAGIC.to_vec();
    header.push(ERRORS_FORMAT_VERSION);
    extend_sparse(&path, &header, MAX_ERRORS_FILE_BYTES + 1);

    let err = io::read_errors_from_file(&path).unwrap_err();
    assert!(err.to_string().contains("cache read cap"), "{err}");
}

#[test]
fn decode_capped_rejects_one_byte_over_the_cap() {
    const CAP: u64 = 64 * 1024;

    let mut out = Vec::new();
    decode_capped(&undeclared_frame(CAP as usize), CAP, &mut out)
        .expect("a body of exactly the cap must decode");
    assert_eq!(out.len() as u64, CAP);

    let mut out = Vec::new();
    let err = decode_capped(&undeclared_frame(CAP as usize + 1), CAP, &mut out).unwrap_err();
    assert!(err.to_string().contains("decompresses past"), "{err}");
    assert!(
        out.len() as u64 <= CAP,
        "the buffer must never grow past the cap, got {}",
        out.len()
    );
}

#[test]
fn decode_capped_accepts_a_body_declaring_exactly_the_cap() {
    // The header check is the one place a `>=` would reject a legitimate cache
    // at the boundary without ever decompressing it, which reads as a permanent
    // miss rather than as an error.
    const CAP: u64 = 4096;

    let mut out = Vec::new();
    decode_capped(&declared_frame(CAP as usize), CAP, &mut out)
        .expect("a declared body of exactly the cap must decode");
    assert_eq!(out.len() as u64, CAP);
}

#[test]
fn decode_capped_bounds_frames_the_header_check_never_saw() {
    // The declared size covers the first frame only, and zstd decodes a
    // concatenation of them. A small honest frame in front of a bomb therefore
    // walks straight past the header check, and the streaming bound is all
    // that is left. This is why that check can only ever short-circuit.
    const CAP: u64 = 64 * 1024;
    let mut stacked = declared_frame(16);
    stacked.extend_from_slice(&undeclared_frame(CAP as usize));

    let mut out = Vec::new();
    let err = decode_capped(&stacked, CAP, &mut out).unwrap_err();
    assert!(err.to_string().contains("decompresses past"), "{err}");
    assert!(
        out.len() as u64 <= CAP,
        "the buffer must never grow past the cap, got {}",
        out.len()
    );
}

#[test]
fn decode_capped_counts_what_a_partial_writer_took() {
    // Every writer the loaders pass takes the whole buffer, so only a short
    // write shows whether the cap is charged what was offered or what landed.
    const CAP: u64 = 4096;

    let mut out = OneByteAtATime(Vec::new());
    decode_capped(&undeclared_frame(CAP as usize), CAP, &mut out)
        .expect("a body of exactly the cap must decode through a short writer");
    assert_eq!(out.0.len() as u64, CAP);
}

#[test]
fn decode_capped_rejects_a_declared_oversize_frame_before_decoding() {
    // The header claims 4 GiB out of a few dozen bytes on disk, which is the
    // whole attack, and it can be answered without decompressing any of it.
    // The specific error is what distinguishes the two bounds: with the header
    // check gone, the streaming bound would let these bytes through and the
    // truncated frame would fail on its missing tail instead.
    let bomb = overstated_frame(4 * 1024 * 1024 * 1024);
    assert!(bomb.len() < 4096, "fixture should be tiny: {}", bomb.len());

    let mut out = Vec::new();
    let err = decode_capped(&bomb, 1024, &mut out).unwrap_err();
    assert!(err.to_string().contains("decompresses past"), "{err}");
    assert!(
        out.is_empty(),
        "a frame refused on its declared size must not write anything"
    );
}
