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
   - `software` (always available, no extra features): software-topology
     detection (`local`/`wsl-host`/`remote`) and live Core-reachability
     probing. `embarch-api`'s `base_url = "auto"` and `embarch-umbrella`'s
     `doctor`/`setup` use this.
   - `hardware` (feature-gated — `features = ["hardware"]`): dev-bench port
     detection, chip hardware-ID readback, enrollment storage, and live
     board-identity validation. `embarch-core` is the one consumer; enabling
     this pulls in `probe-rs`/`serialport`, which `embarch-api`/
     `embarch-umbrella` deliberately never do (matching
     `embarch-umbrella`'s own "no hardware knowledge" boundary).
2. **A thin `embarch-topology` binary** (`bin/`, `features = ["bin"]`): a
   CLI plus a loopback-only local web UI, wrapping the exact same library
   functions, for a human to inspect or fix things directly.

## Depending on this crate

As a path dependency from a sibling repo (the pattern `embarch-study-designer`
already established for this suite):

```toml
embarch-topology = { path = "../embarch-topology" }                       # software-topology only
embarch-topology = { path = "../embarch-topology", features = ["hardware"] } # + hardware topology
```

## Building the CLI/UI

```sh
cargo run --features bin -- --help
cargo run --features bin -- status
cargo run --features bin -- ui
```

## Testing

```sh
cargo test                    # software-topology logic only
cargo test --features hardware
```

No hardware or elevated privileges are needed for `cargo test`. Live
`enroll`/`ui`/`validate` runs do need write access to this crate's own data
directory (`/var/lib/embarch/topology` on Linux/macOS, `%ProgramData%\embarch\topology`
on Windows) — the same machine-wide, admin-owned location `embarch-core`'s
token file already uses, for the same reason (design.md §5's storage-location
decision).
