//! `embarch-topology`: the EmbArch suite's single place for both software
//! topology (where `embarch-core`/`embarch-api` run relative to each other)
//! and hardware topology (what's physically wired to what). Source of
//! truth: `embarch-doc/embarch-topology/spec.md`.
//!
//! `embarch-core`, `embarch-api`, and `embarch-umbrella` all link this crate
//! and call its functions live, in-process, at their own moment of need —
//! no shell-out, no hand-off file, no env var (decisions
//! 2, 3). This crate's own `embarch-topology` binary (`bin/main.rs`, behind
//! the `bin` feature) is a thin CLI wrapper over the exact same
//! functions, for a human to run standalone (decisions 5, 8).
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

/// Gated on `hardware` **or** `wire`: under `wire` alone this module compiles
/// to its plain data types only — the facts Core serves over HTTP — with every
/// function that reads a probe, enumerates a serial port or touches
/// `enrollment.toml` cfg'd out (decision 31). That is what lets a consumer
/// which must never link `probe-rs`/`serialport` still name the types, instead
/// of hand-maintaining a mirror the compiler cannot compare against anything.
#[cfg(any(feature = "hardware", feature = "wire"))]
pub mod hardware;

/// Whether this process is running inside a WSL2 guest. Unconditionally
/// compiled (no feature gate) — zero dependencies beyond `std`, so both
/// halves above can use it without either pulling in the other's
/// dependencies (`wsl2`'s own doc comment; `embarch-topology` decision 27).
#[cfg(any(feature = "software", feature = "hardware"))]
mod wsl2;
