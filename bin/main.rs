//! `embarch-topology`: the thin CLI over this crate's own functions
//! (decision 8) — a human sees exactly what `embarch-core`
//! enforces live, because it's literally the same code.
//!
//! **The local web UI this binary used to also serve (`Ui` subcommand,
//! `bin/ui.rs`) is retired, 2026-08-24** — `embarch-ui` covers the same
//! ground now (decision 5). Every
//! read-only function `bin/ui.rs` called (`list_enrolled`, `recent_alerts`,
//! `list_attached_probes`, etc.) stays right where it was, in `hardware`
//! below — only the page/server that rendered them here is gone.

use clap::{Parser, Subcommand};
use embarch_topology::hardware;
use embarch_topology::software::{self, DEFAULT_CORE_PORT};

#[derive(Parser)]
#[command(name = "embarch-topology", version)]
#[command(about = "EmbArch's software/hardware topology — inspect, enroll, validate")]
struct Cli {
    /// The port a live embarch-core may be listening on, for the guard every
    /// mutating subcommand runs first (decision 28). Core's own `run --port`
    /// can move it off the default; name that port here or the guard looks
    /// in the wrong place and a second writer runs beside Core.
    #[arg(long, global = true, default_value_t = DEFAULT_CORE_PORT)]
    core_port: u16,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Resolve where embarch-core is right now, live — the same call
    /// `embarch-api`'s `base_url = "auto"` makes.
    Status {
        #[arg(long, default_value_t = DEFAULT_CORE_PORT)]
        port: u16,
        /// A declared remote host, if any. It is probed **last**, after
        /// loopback and (under WSL2) the gateway — those two are still tried
        /// first, so a Core on this machine wins over the named host.
        #[arg(long)]
        host: Option<String>,
    },
    /// Detect embarch-dev-bench's serial port, live — the same call
    /// `embarch-core`'s `GET /dev-bench/port` makes.
    DevBench,
    /// List every currently-enrolled board.
    List,
    /// Enroll a debug probe under `role`, reading its live hardware ID as
    /// `chip`. With more than one probe attached, `--probe-serial` picks
    /// which one — omitted, exactly one must be attached.
    ///
    /// `role` is `dut` or `dev-bench` and nothing else (`embarch-ui`
    /// decision 44). What a board is *called* is `--name`, a separate fact:
    /// before it existed the only place to put a board's name was its role,
    /// which is how a bench ended up with a board enrolled as
    /// `client-nucleo`. Omit `--role` to enrol a **bench board** (decision
    /// 41): it holds no role and `--name` is then required, as on Core.
    Enroll {
        #[arg(long, default_value = "")]
        role: String,
        #[arg(long)]
        chip: String,
        #[arg(long)]
        probe_serial: Option<String>,
        /// What this physical board is called — free text, matched against
        /// a firmware repo's `embarch/boards.toml` by the surfaces that
        /// render it, never by this crate.
        #[arg(long, default_value = "")]
        name: String,
    },
    /// Retract whatever board holds `--role`, leaving that role empty, or
    /// (a bench board holds no role) the board enrolled through
    /// `--probe-serial`. Exactly one of the two. Opens no probe: an
    /// unplugged board unenrols exactly like an attached one.
    Unenroll {
        #[arg(long, conflicts_with = "probe_serial", required_unless_present = "probe_serial")]
        role: Option<String>,
        #[arg(long)]
        probe_serial: Option<String>,
    },
    /// Declare which **board type** is in `role` — the shape a repo builds
    /// for (`nrf54l15dk`), not a piece of hardware. Opens no probe: this
    /// half of a role carries no identity claim, which is why it can be
    /// declared with nothing plugged in. `validate` is what reads silicon.
    SetBoard {
        #[arg(long)]
        role: String,
        #[arg(long)]
        board: String,
        /// The probe-rs target that board type attaches as.
        #[arg(long)]
        chip: String,
    },
    /// Re-verify an already-enrolled board's live identity, by role.
    Validate {
        #[arg(long)]
        role: String,
    },
    /// Declare dev-bench's runtime-link USB serial — needed when its link
    /// (a UART bridge) is a different physical USB device from its JTAG
    /// probe, so the JTAG probe's serial can't be used to tell the link
    /// apart from some other SEGGER-VID device on the same bench (e.g. a
    /// DUT's own J-Link). dev-bench must already be enrolled via `enroll
    /// --role dev-bench` first.
    ///
    /// `--interface` answers a different question from `--serial`, and a
    /// two-VCOM probe needs it: both of that probe's ports report the *same*
    /// USB serial, so no serial can tell them apart. The nRF54L15DK is
    /// exactly that case — its `zephyr,console` (`uart20`) is VCOM1,
    /// interface 2, while detection's fallback guess is the lowest interface.
    /// Either flag may be given alone.
    ///
    /// `--clear-serial`/`--clear-interface` unset a previously declared
    /// fact instead of declaring a new one — the fix for the exact failure
    /// decision 27 records: a stale declared serial or interface hard-
    /// narrows detection to a port that no longer exists, and re-enrolling
    /// by role carries it right back over (`validate::enroll`'s own doc
    /// comment on why that's keyed on probe serial), so there was
    /// previously no way to clear it short of hand-editing
    /// `enrollment.toml`.
    SetDevBenchLink {
        #[arg(long)]
        serial: Option<String>,
        #[arg(long)]
        interface: Option<u8>,
        #[arg(long)]
        clear_serial: bool,
        #[arg(long)]
        clear_interface: bool,
    },
    /// Print the most recent topology-mismatch alerts from the durable log.
    Alerts {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

/// Render a detected dev-bench port for a human, **including whether it was
/// guessed** — `spec.md`'s and decision 20's "a caller reports 'COM16,
/// guessed among 2' rather than 'COM16'", verbatim: both state the rendering
/// with a comma, and this is the caller they describe. Until this existed,
/// the CLI printed a guessed port in exactly the shape it printed a
/// determined one — the failure `DetectedPort::guessed_among` was added to
/// end (decision 20).
///
/// Split out of the `DevBench` arm because the guessed shape cannot be
/// reached from a test that runs against real hardware: `guessed_among` is
/// only set by an *under-declared* bench, and this bench declares an
/// interface (spec.md, "Storage and roles"). A pure function over a
/// `DetectedPort` is the only way to assert both shapes without a probe.
///
/// The unguessed rendering is byte-for-byte what it was, deliberately:
/// `embarch-doc/embarch-topology/spec.md` quotes a live 2026-09-06 reading
/// of this command.
fn render_dev_bench_port(port: &hardware::DetectedPort) -> String {
    let guess = match port.guessed_among {
        Some(n) => format!(", guessed among {n}"),
        None => String::new(),
    };
    format!(
        "{}{}\n  detected_by: {}\n  serial: {:?}\n  product: {:?}\n  interface: {:?}\n",
        port.port_name, guess, port.detected_by, port.serial_number, port.product, port.interface
    )
}

/// Render a `validate` failure. Both of this crate's structured errors carry
/// a written-for-a-human `Display`; anything else falls back to `{:?}`,
/// which keeps an unexpected error's context chain rather than flattening
/// it to one line.
///
/// `NotEnrolled` used to fall into that `{:?}` arm — so the one failure a
/// human hits most (the board simply is not enrolled yet) printed a debug
/// struct instead of the sentence that names the fixing command.
fn render_error(e: &anyhow::Error) -> String {
    if let Some(mismatch) = e.downcast_ref::<hardware::TopologyMismatch>() {
        format!("{mismatch}")
    } else if let Some(not_enrolled) = e.downcast_ref::<hardware::NotEnrolled>() {
        format!("{not_enrolled}")
    } else {
        format!("{e:?}")
    }
}

/// Refuses a role outside the fixed pair (`hardware::CANONICAL_ROLES`).
/// The same rule `embarch-core`'s `POST /probes/enroll` applies, stated
/// here too because this binary writes the store directly on a machine with
/// no Core (`refuse_if_core_reachable`) — a check only Core held would be
/// no check at all on exactly the bench that bootstraps itself.
fn check_canonical_role(role: &str) -> anyhow::Result<()> {
    if hardware::is_canonical_role(role) {
        return Ok(());
    }
    if role == hardware::NO_ROLE {
        anyhow::bail!(
            "--role is required here: the roles are {}",
            hardware::CANONICAL_ROLES.join(" and ")
        );
    }
    anyhow::bail!(
        "'{role}' is not a role — the roles are {}. What a board is called goes in --name",
        hardware::CANONICAL_ROLES.join(" and ")
    )
}

/// The role rule `enroll` applies — the one `embarch-core`'s `POST
/// /probes/enroll` applies (decision 35, amended by 61): a canonical role, or
/// the empty role for a bench board (decision 41), which must then carry a
/// name because nothing else tells it from the other bench boards.
fn check_enroll_role(role: &str, name: &str) -> anyhow::Result<()> {
    if role == hardware::NO_ROLE {
        if name.trim().is_empty() {
            anyhow::bail!(
                "a board enrolled with no role is a bench board, and needs a --name to be told \
                 apart — or give it a role: {}",
                hardware::CANONICAL_ROLES.join(" or ")
            );
        }
        return Ok(());
    }
    check_canonical_role(role)
}

/// Refuses a mutation when a live `embarch-core` answers on this machine —
/// `embarch-topology` decision 28 (`decisions/enrollment.md`). `enroll`,
/// `validate` and `set-dev-bench-link` all open a probe or write
/// `enrollment.toml`, and Core already does the identical thing under its
/// own hardware lock (`Arc<Mutex<()>>`, in-process only per
/// `embarch-core/decisions/platform.md:32`) — a second process calling the
/// same function races Core's lock with no queue and no message
/// (`embarch-topology/decisions/enrollment.md:15`,
/// `embarch-core/decisions/enrollment.md:10`), and on a `wsl-host` machine
/// writes a *different* store than the one the Windows-service Core reads
/// (`hardware/paths.rs`).
///
/// `None` here — no candidate answered — is exactly what
/// `embarch-topology status` reports as `core: not found`, and is the one
/// case this function lets the mutation still run in this process: the
/// local-bootstrap machine that has no Core yet to ask, which is why this
/// crate's own functions stay reachable rather than becoming a thin HTTP
/// client (dispatch note, `tasks/topology/011`).
fn refuse_if_core_reachable(
    rt: &tokio::runtime::Runtime,
    core_port: u16,
    endpoint: &str,
) -> anyhow::Result<()> {
    let resolved = rt.block_on(software::resolve_software_topology(core_port, None, None));
    if let Some(w) = resolved.winner {
        anyhow::bail!(
            "refusing: embarch-core is reachable at {} ({}; probed port {core_port}, \
             set --core-port if Core listens elsewhere) and holds the hardware lock and \
             the store this mutation writes (embarch-topology decision 28) — run it there \
             instead, against:\n  {}{endpoint}\nThis command still runs locally when no Core \
             answers at all on port {core_port} (local-bootstrap).",
            w.base_url,
            w.class.as_str(),
            w.base_url,
        );
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let core_port = cli.core_port;

    match cli.command {
        Command::Status { port, host } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let resolved = rt.block_on(software::resolve_software_topology(port, host.as_deref(), None));
            match resolved.winner {
                Some(w) => println!("core: {} ({})", w.base_url, w.class.as_str()),
                None => println!("core: not found"),
            }
            for a in &resolved.attempts {
                println!("  tried {} ({}): {:?}", a.candidate.base_url, a.candidate.class.as_str(), a.outcome);
            }
        }
        Command::DevBench => {
            let port = hardware::resolve_dev_bench_port()?;
            print!("{}", render_dev_bench_port(&port));
        }
        Command::List => {
            for b in hardware::list_enrolled()? {
                let name = if b.name.is_empty() { "(unnamed)" } else { b.name.as_str() };
                print!(
                    "{}: {} probe {} chip {} hardware_id {}",
                    b.role,
                    name,
                    b.probe_serial.as_deref().unwrap_or("(no probe)"),
                    b.chip,
                    b.hardware_id.as_deref().unwrap_or("(unread)")
                );
                if let Some(s) = &b.link_port_serial {
                    print!(" link_port_serial {s}");
                }
                if let Some(i) = b.link_port_interface {
                    print!(" link_port_interface {i}");
                }
                println!();
            }
        }
        Command::Enroll { role, chip, probe_serial, name } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            refuse_if_core_reachable(&rt, core_port, "/probes/enroll")?;
            check_enroll_role(&role, &name)?;
            let board = hardware::enroll(&role, &chip, probe_serial.as_deref(), &name)?;
            println!(
                "enrolled '{}' as role '{}': probe {}, hardware_id {}",
                if board.name.is_empty() { board.chip.as_str() } else { board.name.as_str() },
                board.role,
                board.probe_serial.as_deref().unwrap_or("(no probe)"),
                board.hardware_id.as_deref().unwrap_or("(unread)")
            );
        }
        Command::Unenroll { role, probe_serial } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            refuse_if_core_reachable(&rt, core_port, "/probes/enrolled")?;
            // A role outside the canonical pair is exactly what this
            // command exists to clear, so it is *not* checked here.
            let (removed, what) = match (role, probe_serial) {
                (_, Some(serial)) => {
                    (hardware::unenroll_probe(&serial)?, format!("probe '{serial}'"))
                }
                (Some(role), None) => (hardware::unenroll(&role)?, format!("role '{role}'")),
                (None, None) => unreachable!("clap requires one of --role/--probe-serial"),
            };
            match removed {
                Some(b) => println!(
                    "unenrolled role '{}': was probe {} chip {} hardware_id {}",
                    b.role,
                    b.probe_serial.as_deref().unwrap_or("(no probe)"),
                    b.chip,
                    b.hardware_id.as_deref().unwrap_or("(unread)")
                ),
                None => {
                    eprintln!("no board enrolled under {what}");
                    std::process::exit(1);
                }
            }
        }
        Command::SetBoard { role, board, chip } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            refuse_if_core_reachable(&rt, core_port, "/probes/enrolled/{role}/board")?;
            check_canonical_role(&role)?;
            let row = hardware::set_role_board(&role, &board, &chip)?;
            println!(
                "role '{}' holds board type '{}' (chip {}), probe {}",
                row.role,
                row.name,
                row.chip,
                row.probe_serial.as_deref().unwrap_or("(none yet)")
            );
        }
        Command::Validate { role } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            refuse_if_core_reachable(&rt, core_port, "/validate")?;
            match hardware::validate_role_timed(&role) {
            Ok(v) => println!(
                "ok: '{}' still matches hardware_id {} (enrolled_confirmed_at_utc_ms {}, \
                 validated_at_utc_ms {})",
                v.board.role,
                v.board.hardware_id.as_deref().unwrap_or("(unread)"),
                v.board
                    .confirmed_at_utc_ms
                    .map(|ms| ms.to_string())
                    .unwrap_or_else(|| "(never)".to_string()),
                v.validated_at_utc_ms
            ),
            Err(e) => {
                eprintln!("{}", render_error(&e));
                std::process::exit(1);
            }
        }
        }
        Command::SetDevBenchLink { serial, interface, clear_serial, clear_interface } => {
            if serial.is_some() && clear_serial {
                anyhow::bail!("--serial and --clear-serial are mutually exclusive");
            }
            if interface.is_some() && clear_interface {
                anyhow::bail!("--interface and --clear-interface are mutually exclusive");
            }
            if serial.is_none() && interface.is_none() && !clear_serial && !clear_interface {
                anyhow::bail!(
                    "set-dev-bench-link needs at least one of --serial, --interface, \
                     --clear-serial, --clear-interface"
                );
            }
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            refuse_if_core_reachable(&rt, core_port, "/dev-bench/link")?;
            if let Some(serial) = &serial {
                hardware::set_dev_bench_link_port_serial(serial)?;
                println!("dev-bench link port serial set to '{serial}'");
            }
            if clear_serial {
                hardware::clear_dev_bench_link_port_serial()?;
                println!("dev-bench link port serial cleared");
            }
            if let Some(interface) = interface {
                hardware::set_dev_bench_link_port_interface(interface)?;
                println!("dev-bench link port interface set to {interface}");
            }
            if clear_interface {
                hardware::clear_dev_bench_link_port_interface()?;
                println!("dev-bench link port interface cleared");
            }
        }
        Command::Alerts { limit } => {
            for a in hardware::recent_alerts(limit)? {
                println!(
                    "{} role={} probe={} reason={}",
                    a.occurred_at_utc_ms, a.role, a.probe_serial, a.reason
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port() -> hardware::DetectedPort {
        hardware::DetectedPort {
            port_name: "COM16".into(),
            detected_by: "segger-vid-match".to_string(),
            vendor_id: Some(0x1366),
            product_id: Some(0x1069),
            serial_number: Some("001050288460".into()),
            product: Some("JLink CDC UART Port".into()),
            interface: Some(0),
            guessed_among: None,
        }
    }

    #[test]
    fn enroll_role_matches_cores_rule() {
        assert!(check_enroll_role("dut", "").is_ok());
        assert!(check_enroll_role("dev-bench", "x").is_ok());
        assert!(check_enroll_role("", "pd-partner").is_ok(), "a named bench board enrols");
        let e = check_enroll_role("", "  ").unwrap_err().to_string();
        assert!(e.contains("bench board") && e.contains("--name"), "{e}");
        assert!(check_enroll_role("client-nucleo", "x").is_err());
        assert!(check_enroll_role("DUT", "x").is_err());
    }

    #[test]
    fn set_board_and_validate_still_refuse_the_empty_role() {
        let e = check_canonical_role("").unwrap_err().to_string();
        assert!(e.contains("--role is required"), "{e}");
    }

    /// A Core on a non-default port the operator names: the guard refuses and
    /// says which port it probed. The same Core is invisible on the default.
    #[test]
    fn guard_probes_the_named_port() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, DEFAULT_CORE_PORT);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            }
        });
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let e = refuse_if_core_reachable(&rt, port, "/probes/enroll").unwrap_err().to_string();
        assert!(e.contains(&format!("127.0.0.1:{port}")), "{e}");
        assert!(e.contains(&format!("probed port {port}")), "{e}");
    }

    #[test]
    fn determined_port_renders_unchanged() {
        assert_eq!(
            render_dev_bench_port(&port()),
            "COM16\n  detected_by: segger-vid-match\n  serial: Some(\"001050288460\")\n  \
             product: Some(\"JLink CDC UART Port\")\n  interface: Some(0)\n"
        );
    }

    #[test]
    fn guessed_port_says_so_on_the_port_name_line() {
        let mut p = port();
        p.guessed_among = Some(2);
        let rendered = render_dev_bench_port(&p);
        assert!(
            rendered.starts_with("COM16, guessed among 2\n"),
            "guess must be on the port-name line, the one a caller copies: {rendered}"
        );
        // Everything else is untouched, so the two shapes differ only by the
        // guess — that is what makes a diff of two runs readable.
        assert_eq!(
            rendered.replace(", guessed among 2", ""),
            render_dev_bench_port(&port())
        );
    }

    #[test]
    fn not_enrolled_renders_its_sentence_not_its_debug() {
        let e = anyhow::Error::new(hardware::NotEnrolled { role: "dut".into() });
        let rendered = render_error(&e);
        assert_eq!(rendered, e.downcast_ref::<hardware::NotEnrolled>().unwrap().to_string());
        assert!(rendered.starts_with("no board enrolled under role 'dut'"), "{rendered}");
        assert!(!rendered.contains("NotEnrolled {"), "debug shape leaked: {rendered}");
    }

    #[test]
    fn mismatch_still_renders_its_display() {
        let e = anyhow::Error::new(hardware::TopologyMismatch {
            role: "dev-bench".into(),
            probe_serial: "001050288460".into(),
            chip: "nrf54l15".into(),
            recorded_hardware_id: "recorded".into(),
            live_hardware_id: Some("live".into()),
            reason: "hardware_id changed".into(),
            fix_it_url: "http://127.0.0.1:8765/topology".into(),
        });
        let mismatch = e.downcast_ref::<hardware::TopologyMismatch>().unwrap();
        assert_eq!(render_error(&e), mismatch.to_string());
    }

    #[test]
    fn an_unrecognised_error_keeps_its_context_chain() {
        let e = anyhow::anyhow!("inner").context("outer");
        let rendered = render_error(&e);
        assert!(rendered.contains("outer") && rendered.contains("inner"), "{rendered}");
    }
}
