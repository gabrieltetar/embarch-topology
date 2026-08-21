//! `embarch-topology`: the EmbArch suite's single place for both software
//! topology (where `embarch-core`/`embarch-api` run relative to each other)
//! and hardware topology (what's physically wired to what). Source of
//! truth: `embarch-doc/embarch-topology/design.md`.
//!
//! `embarch-core`, `embarch-api`, and `embarch-umbrella` all link this crate
//! and call its functions live, in-process, at their own moment of need —
//! no shell-out, no hand-off file, no env var (design.md §2, §3 decisions
//! 2, 3). This crate's own `embarch-topology` binary (`bin/main.rs`, behind
//! the `bin` feature) is a thin CLI/UI wrapper over the exact same
//! functions, for a human to run standalone (design.md §3 decisions 5, 8).
//!
//! Two independent halves, split across a feature boundary that mirrors a
//! real architectural boundary already in the suite
//! (`embarch-umbrella/Cargo.toml`'s "deliberately absent: probe-rs and
//! serialport" comment):
//!
//! - [`software`] — behind the `software` feature (on by default; implied
//!   by `bin`). Software-topology-class detection (`local`/`wsl-host`/
//!   `remote`) and Core-reachability probing, needing `reqwest`/`tokio`.
//!   This is what `embarch-api`/`embarch-umbrella` use.
//! - [`hardware`] — behind the `hardware` feature (implied by `bin`).
//!   Dev-bench port detection, chip hardware-ID readback, enrollment
//!   storage, and live board-identity validation. Needs `probe-rs`/
//!   `serialport`; `embarch-core` is the one consumer that turns this on —
//!   and, since it never calls into `software` at all, is also the one
//!   consumer that opts out of it (`default-features = false, features =
//!   ["hardware"]`), so its own Windows build never has to compile
//!   `reqwest`'s transitive `aws-lc-sys` (a real C-toolchain dependency
//!   neither Core nor its cross-compilation story has any use for).

#[cfg(feature = "software")]
pub mod software;

#[cfg(feature = "hardware")]
pub mod hardware;
