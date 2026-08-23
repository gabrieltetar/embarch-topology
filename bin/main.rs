//! `embarch-topology`: the thin CLI + local web UI over this crate's own
//! functions (design.md §3 decisions 5, 8) — a human sees exactly what
//! `embarch-core` enforces live, because it's literally the same code.

mod ui;

use clap::{Parser, Subcommand};
use embarch_topology::hardware;
use embarch_topology::software::{self, DEFAULT_CORE_PORT};

#[derive(Parser)]
#[command(name = "embarch-topology", version)]
#[command(about = "EmbArch's software/hardware topology — inspect, enroll, validate, serve a local UI")]
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
    /// Print the most recent topology-mismatch alerts from the durable log.
    Alerts {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Serve the local web UI (design.md §3 decision 5) — loopback-only,
    /// for the duration of this process. Ctrl-C to stop.
    Ui {
        #[arg(long, default_value_t = hardware::DEFAULT_UI_PORT)]
        port: u16,
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
                println!("{}: probe {} chip {} hardware_id {}", b.role, b.probe_serial, b.chip, b.hardware_id);
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
        Command::Alerts { limit } => {
            for a in hardware::recent_alerts(limit)? {
                println!(
                    "{} role={} probe={} reason={}",
                    a.occurred_at_utc_ms, a.role, a.probe_serial, a.reason
                );
            }
        }
        Command::Ui { port } => {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            rt.block_on(ui::serve(port))?;
        }
    }

    Ok(())
}
