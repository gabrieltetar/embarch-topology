//! The local web UI (design.md §3 decisions 5, 12): a thin wrapper over
//! this crate's own functions, served only while this process runs, bound
//! loopback-only.
//!
//! **Read-only, entirely — enrolling a board happens through `embarch-
//! core`'s own `GET /enroll` instead (design.md §3 decision 14, reversed
//! 2026-08-23; `embarch-core/design.md` §3 decision 25).** This page
//! originally grew a `POST /enroll` form of its own, calling the same
//! `hardware::enroll` Core's `POST /probes/enroll` already calls — real,
//! correct code, but a second process independently doing the exact
//! hardware I/O and file write Core already does with its own `hw_lock`
//! held. Reversed once that redundancy was pointed out: enrollment now has
//! exactly one route to it (Core's), not two that happen to agree today and
//! could silently drift apart tomorrow — the same "one implementation,
//! multiple call sites, not two independent layers" principle decision 8
//! already established for `validate()`, extended here to `enroll()`. This
//! page keeps showing what's enrolled/attached/alerted, live, for whenever
//! Core isn't the thing in front of you — it just doesn't let you change
//! anything anymore.

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, IntoResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use futures_util::StreamExt;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;

use embarch_topology::hardware::{self, Alert, AttachedProbe, EnrolledBoard};
use embarch_topology::software::{self, ResolvedSoftwareTopology};

#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<Alert>,
}

/// Binds loopback-only (matching `embarch-core/design.md` §3 decision 6's
/// amendment reasoning: no TLS, no reason to expose past localhost), writes
/// the marker file `hardware::alert::push_live` looks for, and removes it
/// again on a clean Ctrl-C shutdown — a stale leftover marker just means a
/// future push attempt fails harmlessly against whatever's since bound that
/// port (`alert.rs`'s own doc comment already accepts that risk).
pub async fn serve(port: u16) -> anyhow::Result<()> {
    let (tx, _rx) = broadcast::channel(64);
    let state = Arc::new(AppState { tx });

    let app = Router::new()
        .route("/", get(index))
        .route("/mismatch/{id}", get(mismatch))
        .route("/events", get(events))
        .route("/_internal/alert", post(receive_alert))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    let marker = hardware::ui_marker_path()?;
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&marker, &addr)?;
    println!("embarch-topology UI listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;

    let _ = std::fs::remove_file(&marker);
    result.map_err(anyhow::Error::from)
}

async fn receive_alert(State(state): State<Arc<AppState>>, Json(alert): Json<Alert>) -> impl IntoResponse {
    let _ = state.tx.send(alert);
    axum::http::StatusCode::NO_CONTENT
}

async fn events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.tx.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| async move {
        match item {
            Ok(alert) => serde_json::to_string(&alert)
                .ok()
                .map(|json| Ok(Event::default().data(json))),
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn page(body: String) -> Html<String> {
    Html(format!(
        "<!doctype html><html><head><title>embarch-topology</title>\
         <meta charset=\"utf-8\"></head><body style=\"font-family:monospace;max-width:60rem;margin:2rem auto\">\
         <h1>embarch-topology</h1>{body}\
         <script>\
           const es = new EventSource('/events');\
           es.onmessage = (e) => {{ \
             const div = document.createElement('pre'); \
             div.textContent = 'LIVE ALERT: ' + e.data; \
             div.style.color = 'red'; \
             document.body.prepend(div); \
           }};\
         </script>\
         </body></html>"
    ))
}

/// `{base_url}/enroll`, plus an optional `?role=` so a click lands with
/// that role's drop zone already highlighted (`embarch-core/design.md`'s
/// `enroll_page.rs` reads it client-side). `role` is only ever one of this
/// crate's own two hardware roles, never user input, so no query-escaping
/// is needed beyond the existing `html_escape` every caller already runs
/// the whole `<a>` through.
fn enroll_link(base_url: &str, role: Option<&str>) -> String {
    match role {
        Some(r) => format!("{base_url}/enroll?role={r}"),
        None => format!("{base_url}/enroll"),
    }
}

/// Best-effort link to Core's own `/enroll` page — this is guidance for a
/// human, not something anything here depends on being right: if Core
/// isn't reachable, this just falls back to plain text naming the endpoint
/// instead of a clickable link. Takes the already-resolved topology so a
/// page render only ever probes Core once, not once per link it draws.
fn core_enroll_link(resolved: &ResolvedSoftwareTopology) -> String {
    match resolved.base_url() {
        Some(base) => {
            let href = enroll_link(base, None);
            format!("<a href=\"{href}\">{href}</a>")
        }
        None => "<i>embarch-core not reachable right now — its own `/enroll` page is where boards get enrolled</i>"
            .to_string(),
    }
}

/// Whether `role` has a currently-attached probe whose serial matches what's
/// enrolled for it — the same by-serial correlation `index()`'s enrolled/
/// attached lists already let a human do by eye, just precomputed for the
/// diagram's coloring. Deliberately **not** a live identity check: that's
/// `validate()`'s job (`hardware::validate_role`, exposed over HTTP as
/// Core's own `POST /validate`) and stays a call a human or agent makes on
/// purpose, not something a page render triggers against real hardware on
/// every poll.
fn role_attached(board: &EnrolledBoard, attached: &[AttachedProbe]) -> bool {
    attached
        .iter()
        .any(|p| p.serial_number.as_deref() == Some(board.probe_serial.as_str()))
}

/// A small inline SVG summarizing both topology kinds at a glance: this
/// process's own route to Core (software topology), and Core's two
/// hardware roles (dev-bench, dut) colored by enrolled/attached state.
/// §5's "no topology diagram" gap (`milestone-1.md` item 6). Deliberately
/// static markup built from data `index()` already fetched — no new
/// hardware I/O, no new HTTP calls, just a picture of what the lists below
/// it already say in words.
fn topology_diagram(resolved: &ResolvedSoftwareTopology, boards: &[EnrolledBoard], attached: &[AttachedProbe]) -> String {
    let core_reachable = resolved.winner.is_some();
    let core_label = match resolved.base_url() {
        Some(base) => html_escape(base),
        None => "not reachable".to_string(),
    };
    let (core_fill, core_stroke) = if core_reachable { ("#eef8ee", "#070") } else { ("#fdecec", "#b00") };

    let role_box = |role: &str, x: i32| -> String {
        let board = boards.iter().find(|b| b.role == role);
        let (fill, stroke, status) = match board {
            Some(b) if role_attached(b, attached) => ("#eef8ee", "#070", format!("chip {}", html_escape(&b.chip))),
            Some(b) => ("#fdf6e3", "#b8860b", format!("chip {} — not attached", html_escape(&b.chip))),
            None => ("#f2f2f2", "#999", "not enrolled".to_string()),
        };
        // `r##"..."##`, not `r#"..."#`: the content itself contains `"#`
        // (every `fill="#rrggbb"`), which would otherwise close the raw
        // string right there.
        format!(
            r##"<rect x="{x}" y="120" width="170" height="54" rx="8" fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>
<text x="{cx}" y="142" text-anchor="middle" font-weight="bold">{role}</text>
<text x="{cx}" y="160" text-anchor="middle" font-size="11">{status}</text>"##,
            cx = x + 85,
        )
    };

    // Same `r##"..."##` reasoning as `role_box` above.
    format!(
        r##"<svg viewBox="0 0 460 200" width="460" height="200" style="font-family:monospace;font-size:12px">
<rect x="10" y="20" width="150" height="50" rx="8" fill="#f2f2f2" stroke="#666" stroke-width="1.5"/>
<text x="85" y="50" text-anchor="middle">this UI process</text>
<line x1="160" y1="45" x2="230" y2="45" stroke="#666" stroke-width="1.5" marker-end="url(#arrow)"/>
<rect x="230" y="20" width="220" height="50" rx="8" fill="{core_fill}" stroke="{core_stroke}" stroke-width="1.5"/>
<text x="340" y="42" text-anchor="middle" font-weight="bold">embarch-core</text>
<text x="340" y="60" text-anchor="middle" font-size="11">{core_label}</text>
<line x1="340" y1="70" x2="180" y2="120" stroke="#666" stroke-width="1.5" marker-end="url(#arrow)"/>
<line x1="340" y1="70" x2="380" y2="120" stroke="#666" stroke-width="1.5" marker-end="url(#arrow)"/>
{dev_bench_box}
{dut_box}
<defs><marker id="arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
<path d="M0 0L10 5L0 10z" fill="#666"/></marker></defs>
</svg>"##,
        dev_bench_box = role_box(hardware::DEV_BENCH_ROLE, 10),
        dut_box = role_box("dut", 290),
    )
}

async fn index() -> Html<String> {
    let boards = hardware::list_enrolled().unwrap_or_default();
    let alerts = hardware::recent_alerts(20).unwrap_or_default();
    let attached = hardware::list_attached_probes();
    let resolved = software::resolve_software_topology(software::DEFAULT_CORE_PORT, None, None).await;

    let mut body = String::new();

    body.push_str("<h2>Topology at a glance</h2>");
    body.push_str(&topology_diagram(&resolved, &boards, &attached));

    body.push_str("<h2>Enrolled boards (hardware topology)</h2><ul>");
    for b in &boards {
        body.push_str(&format!(
            "<li>role <b>{}</b> — chip {} — probe {} — hardware_id {}</li>",
            html_escape(&b.role), html_escape(&b.chip), html_escape(&b.probe_serial), html_escape(&b.hardware_id)
        ));
    }
    if boards.is_empty() {
        body.push_str("<li><i>none enrolled yet</i></li>");
    }
    body.push_str("</ul>");

    // Read-only, deliberately — see this file's own module doc comment for
    // why enrolling happens through embarch-core's `/enroll` now, not here.
    body.push_str("<h2>Currently attached</h2>");
    body.push_str(&format!(
        "<p>{}</p>",
        if attached.is_empty() {
            "<i>no debug probes detected</i>".to_string()
        } else {
            attached
                .iter()
                .map(|p| {
                    format!(
                        "{} (serial {})",
                        html_escape(&p.identifier),
                        html_escape(p.serial_number.as_deref().unwrap_or("none"))
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    body.push_str(&format!("<p>To enroll a board: {}</p>", core_enroll_link(&resolved)));

    body.push_str("<h2>Recent alerts</h2><ul>");
    for a in alerts.iter().rev() {
        // A per-alert "re-enroll the board this alert is about" action,
        // linking straight into Core's `/enroll` with the role pre-filled —
        // §5's other named UI gap (`milestone-1.md` item 6). Only drawn when
        // Core is actually reachable right now; a dead link back to an
        // unreachable Core would be worse than the plain-text fallback
        // `core_enroll_link` already uses for the same reason.
        let reenroll = match resolved.base_url() {
            Some(base) => {
                let href = html_escape(&enroll_link(base, Some(&a.role)));
                format!(" — <a href=\"{href}\">re-enroll {}</a>", html_escape(&a.role))
            }
            None => String::new(),
        };
        body.push_str(&format!(
            "<li><a href=\"/mismatch/{}\">{}</a> — role {} — {}{}</li>",
            html_escape(&a.id), a.occurred_at_utc_ms, html_escape(&a.role), html_escape(&a.reason), reenroll
        ));
    }
    if alerts.is_empty() {
        body.push_str("<li><i>none logged</i></li>");
    }
    body.push_str("</ul>");

    page(body)
}

async fn mismatch(Path(id): Path<String>) -> Html<String> {
    let alerts = hardware::recent_alerts(500).unwrap_or_default();
    match alerts.into_iter().find(|a| a.id == id) {
        Some(a) => {
            // Closes the last piece of §5's "click-to-fix flow" gap: a
            // `fix_it_url` landing here (design.md §3 decision 12's own
            // "relay, don't auto-open" `TopologyMismatch` field) previously
            // dead-ended at this JSON dump. One more hop, same "only when
            // Core is actually reachable" posture as `index()`'s own
            // per-alert link.
            let resolved = software::resolve_software_topology(software::DEFAULT_CORE_PORT, None, None).await;
            let reenroll = match resolved.base_url() {
                Some(base) => {
                    let href = html_escape(&enroll_link(base, Some(&a.role)));
                    format!("<p><a href=\"{href}\">Re-enroll {}</a></p>", html_escape(&a.role))
                }
                None => String::new(),
            };
            page(format!(
                "<h2>Mismatch {}</h2>{}<pre>{}</pre>",
                html_escape(&a.id),
                reenroll,
                html_escape(&serde_json::to_string_pretty(&a).unwrap_or_default())
            ))
        }
        None => page(format!("<p>No alert with id {} in the durable log.</p>", html_escape(&id))),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(role: &str, probe_serial: &str) -> EnrolledBoard {
        EnrolledBoard {
            probe_serial: probe_serial.to_string(),
            role: role.to_string(),
            chip: "nRF54L15".to_string(),
            hardware_id: "0xdeadbeef".to_string(),
            confirmed_at_utc_ms: 0,
            link_port_serial: None,
        }
    }

    fn attached(serial: &str) -> AttachedProbe {
        AttachedProbe {
            identifier: "J-Link".to_string(),
            vendor_id: 0x1366,
            product_id: 0x0105,
            serial_number: Some(serial.to_string()),
        }
    }

    #[test]
    fn enroll_link_without_role_is_bare() {
        assert_eq!(enroll_link("http://127.0.0.1:4884", None), "http://127.0.0.1:4884/enroll");
    }

    #[test]
    fn enroll_link_with_role_prefills_the_query_param() {
        assert_eq!(
            enroll_link("http://127.0.0.1:4884", Some("dev-bench")),
            "http://127.0.0.1:4884/enroll?role=dev-bench"
        );
    }

    #[test]
    fn role_attached_true_when_a_probe_serial_matches() {
        let b = board("dut", "12345");
        assert!(role_attached(&b, &[attached("00000"), attached("12345")]));
    }

    #[test]
    fn role_attached_false_when_no_probe_serial_matches() {
        let b = board("dut", "12345");
        assert!(!role_attached(&b, &[attached("00000")]));
    }

    #[test]
    fn role_attached_false_with_nothing_attached() {
        let b = board("dut", "12345");
        assert!(!role_attached(&b, &[]));
    }

    fn unresolved() -> ResolvedSoftwareTopology {
        ResolvedSoftwareTopology { winner: None, attempts: vec![] }
    }

    fn resolved(base_url: &str) -> ResolvedSoftwareTopology {
        ResolvedSoftwareTopology {
            winner: Some(software::Candidate { class: software::TopologyClass::Local, base_url: base_url.to_string() }),
            attempts: vec![],
        }
    }

    #[test]
    fn diagram_shows_core_unreachable_and_both_roles_unenrolled() {
        let svg = topology_diagram(&unresolved(), &[], &[]);
        assert!(svg.contains("not reachable"));
        assert!(svg.contains(r##"fill="#fdecec""##)); // Core box, unreachable
        assert!(svg.matches("not enrolled").count() == 2); // dev-bench and dut, both unenrolled
        assert!(svg.matches(r##"fill="#f2f2f2""##).count() >= 2); // "this UI process" plus both unenrolled role boxes
    }

    #[test]
    fn diagram_shows_an_attached_enrolled_role_as_green() {
        let b = board(hardware::DEV_BENCH_ROLE, "12345");
        let svg = topology_diagram(&resolved("http://127.0.0.1:4884"), &[b], &[attached("12345")]);
        assert!(svg.contains("http://127.0.0.1:4884"));
        assert!(svg.contains("chip nRF54L15"));
        assert!(svg.contains(r##"fill="#eef8ee""##)); // reachable Core + attached role share this fill
        assert!(!svg.contains("not attached"));
    }

    #[test]
    fn diagram_shows_an_enrolled_but_unattached_role_as_amber() {
        let b = board("dut", "12345");
        let svg = topology_diagram(&resolved("http://127.0.0.1:4884"), &[b], &[]);
        assert!(svg.contains("not attached"));
        assert!(svg.contains(r##"fill="#fdf6e3""##));
    }
}
