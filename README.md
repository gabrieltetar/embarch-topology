# embarch-topology

The EmbArch suite's single place for both software topology (where
`embarch-core`/`embarch-api` run relative to each other — local/wsl-host/
remote) and hardware topology (what's physically wired to what — dev-bench's
port, board identity, probe enrollment).

Design doc (source of truth): [`embarch-doc/embarch-topology/design.md`](../embarch-doc/embarch-topology/design.md).

## What this is

Two things built from one codebase:

1. **A shared Rust library** (`src/`) that `embarch-core`, `embarch-api`, and
   `embarch-umbrella` all depend on as a plain path dependency and call
   live, in-process, at their own moment of need — no shell-out, no
   hand-off file, no env var.
   - `software` (**on by default**): software-topology detection
     (`local`/`wsl-host`/`remote`) and live Core-reachability probing, via
     `reqwest`/`tokio`. `embarch-api`'s `base_url = "auto"` and
     `embarch-umbrella`'s `doctor`/`setup` use this.
   - `hardware` (opt-in — `features = ["hardware"]`): dev-bench port
     detection, chip hardware-ID readback, enrollment storage, and live
     board-identity validation. `embarch-core` is the one consumer; enabling
     this pulls in `probe-rs`/`serialport`, which `embarch-api`/
     `embarch-umbrella` deliberately never do (matching
     `embarch-umbrella`'s own "no hardware knowledge" boundary).
     `embarch-core` never calls into `software` either, so it opts out of
     that with `default-features = false` — its own real dependency tree
     (`cargo tree --no-default-features --features hardware`) carries
     neither `reqwest` nor its transitive `aws-lc-sys` (a real C-toolchain
     dependency, irrelevant to Core's own hardware-facing job).
2. **A thin `embarch-topology` binary** (`bin/`, `features = ["bin"]`): a
   CLI wrapping the exact same library functions, for a human to inspect or
   fix things directly. Used to also serve a loopback-only local web UI
   (`ui` subcommand) — retired 2026-08-24 in favor of `embarch-ui`'s
   Topology tab, which covers the same ground over `embarch-core`'s HTTP
   API instead (`embarch-doc/embarch-ui/milestone-1.md` §4.9).

## Depending on this crate

As a path dependency from a sibling repo (the pattern `embarch-study-designer`
already established for this suite):

```toml
embarch-topology = { path = "../embarch-topology" }                                                    # software-topology only (default)
embarch-topology = { path = "../embarch-topology", default-features = false, features = ["hardware"] } # hardware-topology only (embarch-core)
```

## Building the CLI

```sh
cargo run --features bin -- --help
cargo run --features bin -- status
```

## Testing

```sh
cargo test                                             # software-topology logic only (default)
cargo test --no-default-features --features hardware   # embarch-core's real config
cargo test --features hardware                         # both, default features still on
```

No hardware or elevated privileges are needed for `cargo test`. Live
`enroll`/`validate` runs do need write access to this crate's own data
directory (`/var/lib/embarch/topology` on Linux/macOS, `%ProgramData%\embarch\topology`
on Windows) — the same machine-wide, admin-owned location `embarch-core`'s
token file already uses, for the same reason (design.md §5's storage-location
decision).
