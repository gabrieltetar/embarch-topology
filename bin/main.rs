//! `embarch-topology`: the thin CLI over this crate's own functions
//! (design.md §3 decision 8) — a human sees exactly what `embarch-core`
//! enforces live, because it's literally the same code.
//!
//! **The local web UI this binary used to also serve (`Ui` subcommand,
//! `bin/ui.rs`) is retired, 2026-08-24** — `embarch-ui` covers the same
//! ground now (`embarch-doc/embarch-ui/milestone-1.md` §4.9). Every
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
    SetDevBenchLink {
        #[arg(long)]
        serial: String,
    },
    /// Print the most recent topology-mismatch alerts from the durable log.
    Alerts {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
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
            println!("{}", port.port_name);
            println!(
                "  detected_by: {}\n  serial: {:?}\n  product: {:?}\n  interface: {:?}",
                port.detected_by, port.serial_number, port.product, port.interface
            );
        }
        Command::List => {
            for b in hardware::list_enrolled()? {
                print!("{}: probe {} chip {} hardware_id {}", b.role, b.probe_serial, b.chip, b.hardware_id);
                match &b.link_port_serial {
                    Some(s) => println!(" link_port_serial {s}"),
                    None => println!(),
                }
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
                if let Some(mismatch) = e.downcast_ref::<hardware::TopologyMismatch>() {
                    eprintln!("{mismatch}");
                } else {
                    eprintln!("{e:?}");
                }
                std::process::exit(1);
            }
        },
        Command::SetDevBenchLink { serial } => {
            hardware::set_dev_bench_link_port_serial(&serial)?;
            println!("dev-bench link port serial set to '{serial}'");
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
