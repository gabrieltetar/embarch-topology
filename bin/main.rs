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
        /// A declared remote host, if any (skips auto-detection's WSL2/
        /// loopback candidates and probes only this one).
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
    Enroll {
        #[arg(long)]
        role: String,
        #[arg(long)]
        chip: String,
        #[arg(long)]
        probe_serial: Option<String>,
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
    SetDevBenchLink {
        #[arg(long)]
        serial: Option<String>,
        #[arg(long)]
        interface: Option<u8>,
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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

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
                print!("{}: probe {} chip {} hardware_id {}", b.role, b.probe_serial, b.chip, b.hardware_id);
                if let Some(s) = &b.link_port_serial {
                    print!(" link_port_serial {s}");
                }
                if let Some(i) = b.link_port_interface {
                    print!(" link_port_interface {i}");
                }
                println!();
            }
        }
        Command::Enroll { role, chip, probe_serial } => {
            let board = hardware::enroll(&role, &chip, probe_serial.as_deref())?;
            println!(
                "enrolled '{}' as role '{}': probe {}, hardware_id {}",
                board.chip, board.role, board.probe_serial, board.hardware_id
            );
        }
        Command::Validate { role } => match hardware::validate_role(&role) {
            Ok(board) => println!("ok: '{}' still matches hardware_id {}", board.role, board.hardware_id),
            Err(e) => {
                eprintln!("{}", render_error(&e));
                std::process::exit(1);
            }
        },
        Command::SetDevBenchLink { serial, interface } => {
            if serial.is_none() && interface.is_none() {
                anyhow::bail!("set-dev-bench-link needs at least one of --serial or --interface");
            }
            if let Some(serial) = &serial {
                hardware::set_dev_bench_link_port_serial(serial)?;
                println!("dev-bench link port serial set to '{serial}'");
            }
            if let Some(interface) = interface {
                hardware::set_dev_bench_link_port_interface(interface)?;
                println!("dev-bench link port interface set to {interface}");
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
            detected_by: "segger-vid-match",
            vendor_id: Some(0x1366),
            product_id: Some(0x1069),
            serial_number: Some("001050288460".into()),
            product: Some("JLink CDC UART Port".into()),
            interface: Some(0),
            guessed_among: None,
        }
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
