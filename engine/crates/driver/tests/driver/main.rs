//! Smoke tests for the shared driver pipeline (Session + primitives).
//!
//! The driver is the anti-drift hub between the CLI and the LSP: both call its
//! Session/pipeline primitives so the load sequence can't diverge. These tests
//! pin the pipeline against the checked-in `performancetest2` fixture (a
//! Stellaris mod slice, validated with the pinned upstream ruleset) plus a
//! couple of synthesized temp dirs for the discovery-config helper. They assert
//! the pipeline loads, indexes, and validates without panicking, and that its
//! output is deterministic across runs.

mod common;
mod discovery;
mod loc;
mod parse_cache;
mod session;
mod unused_cw239;
mod vanilla_cache;
