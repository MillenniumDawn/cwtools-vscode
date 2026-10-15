use crate::common::cwtools;
use predicates::prelude::*;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};

// ── Help / version ───────────────────────────────────────────────────────────

#[test]
fn test_help_exits_with_usage() {
    cwtools()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("cwtools"))
        .stdout(predicate::str::contains("Usage"));
}

#[test]
fn test_version_flag_prints_crate_version() {
    // Same source as `cwtools-server --version` (CARGO_PKG_VERSION), so the two
    // binaries can't report different versions.
    cwtools()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn test_version_short_flag_prints_crate_version() {
    cwtools()
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

// ── Code catalog (explain / list-codes) ──────────────────────────────────────

#[test]
fn test_explain_prints_the_reference_row() {
    cwtools()
        .args(["explain", "CW113"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW113  Error  MissingFile"))
        .stdout(predicate::str::contains(
            "Message  File {} not found, this is case sensitive",
        ))
        .stdout(predicate::str::contains("Meaning  A file path referenced"))
        .stdout(predicate::str::contains("Status   Emitted"))
        .stdout(predicate::str::contains("ERROR_CODES.md#cw113"));
}

#[test]
fn test_explain_accepts_a_lowercase_code() {
    cwtools()
        .args(["explain", "cw113"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW113  Error  MissingFile"));
}

#[test]
fn test_explain_unknown_code_is_a_usage_error() {
    cwtools()
        .args(["explain", "CW999"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("CW999"))
        .stderr(predicate::str::contains("list-codes"));
}

#[test]
fn test_list_codes_covers_the_catalog_and_marks_the_pending_ones() {
    cwtools()
        .args(["list-codes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW001  Error"))
        .stdout(predicate::str::contains("CW107  Information"))
        // A pass-through template ("{}") renders as the code's prose name.
        .stdout(predicate::str::contains(
            "CW240  Error        Unexpected value",
        ))
        .stdout(
            predicate::str::contains("CW220").and(predicate::str::contains("(emission pending)")),
        )
        // CW274's check shipped, so it must not read as pending.
        .stdout(
            predicate::str::contains("CW274  Error        InlineScriptError  (emission pending)")
                .not(),
        );
}

// ── Shell completions ────────────────────────────────────────────────────────

#[test]
fn test_completions_generate_for_every_supported_shell() {
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        cwtools()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("cwtools"));
    }
}

/// Generated from the live clap definition, so a flag that exists is offered.
#[test]
fn test_completions_cover_the_subcommands_and_flags() {
    cwtools()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("validate"))
        .stdout(predicate::str::contains("--fail-on"))
        .stdout(predicate::str::contains("list-codes"))
        .stdout(predicate::str::contains("format"));
}

#[test]
fn test_completions_unknown_shell_fails() {
    cwtools()
        .args(["completions", "tcsh"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("tcsh"));
}

#[cfg(unix)]
#[test]
fn test_closed_stdout_does_not_panic() {
    let (read_peer, write_peer) = UnixStream::pair().unwrap();
    drop(read_peer);
    let write_peer: OwnedFd = write_peer.into();

    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_cwtools"));
    command
        .arg("list-codes")
        .env("RUST_LOG", "")
        .stdout(Stdio::from(write_peer));
    let status = command.spawn().unwrap().wait().unwrap();

    assert_eq!(
        status.signal(),
        Some(libc::SIGPIPE),
        "unexpected exit status: {status:?}"
    );
}

// ── Error handling ───────────────────────────────────────────────────────────

#[test]
fn test_unknown_engine_fails() {
    cwtools()
        .args(["--engine", "fortran", "parse", "somefile"])
        .assert()
        .failure();
}

#[test]
fn test_no_subcommand_fails() {
    cwtools().assert().failure();
}
