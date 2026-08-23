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

use embarch_topology::hardware::{self, Alert};
use embarch_topology::software;

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

/// Best-effort link to Core's own `/enroll` page — this is guidance for a
/// human, not something anything here depends on being right: if Core
/// isn't reachable (or reachable only over a topology this crate's own
/// `resolve_software_topology` hasn't been pointed at), this just falls
/// back to plain text naming the endpoint instead of a clickable link.
async fn core_enroll_link() -> String {
    let resolved = software::resolve_software_topology(software::DEFAULT_CORE_PORT, None, None).await;
    match resolved.winner {
        Some(w) => format!("<a href=\"{}/enroll\">{}/enroll</a>", w.base_url, w.base_url),
        None => "<i>embarch-core not reachable right now — its own `/enroll` page is where boards get enrolled</i>"
            .to_string(),
    }
}

async fn index() -> Html<String> {
    let boards = hardware::list_enrolled().unwrap_or_default();
    let alerts = hardware::recent_alerts(20).unwrap_or_default();
    let attached = hardware::list_attached_probes();

    let mut body = String::new();

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
    body.push_str(&format!("<p>To enroll a board: {}</p>", core_enroll_link().await));

    body.push_str("<h2>Recent alerts</h2><ul>");
    for a in alerts.iter().rev() {
        body.push_str(&format!(
            "<li><a href=\"/mismatch/{}\">{}</a> — role {} — {}</li>",
            html_escape(&a.id), a.occurred_at_utc_ms, html_escape(&a.role), html_escape(&a.reason)
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
        Some(a) => page(format!(
            "<h2>Mismatch {}</h2><pre>{}</pre>",
            html_escape(&a.id),
            html_escape(&serde_json::to_string_pretty(&a).unwrap_or_default())
        )),
        None => page(format!("<p>No alert with id {} in the durable log.</p>", html_escape(&id))),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
