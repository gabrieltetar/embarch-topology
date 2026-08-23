//! The local web UI (design.md §3 decisions 5, 12): a thin wrapper over
//! this crate's own functions, served only while this process runs, bound
//! loopback-only.
//!
//! **Hardware topology is the one interactive part — software topology
//! stays read-only, deliberately (design.md §3 decision 25).** Enrolling a
//! board (`/enroll`, below) is the one step in this whole suite that
//! structurally *requires* a human: no software can infer which physical
//! board a probe is wired to, only a person physically isolating it and
//! saying so. Everything else this page shows — resolved software
//! topology, dev-bench port detection, live validation — is already fully
//! automatic, so there's nothing to *submit* for it, only to *watch*.

use axum::extract::{Form, Path, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, IntoResponse, Redirect, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use futures_util::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;

use embarch_topology::hardware::{self, Alert};

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
        .route("/enroll", post(enroll_form))
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

#[derive(Deserialize)]
struct EnrollFormBody {
    role: String,
    chip: String,
}

/// Handles the one interactive form this UI has (module doc comment). Runs
/// `hardware::enroll` inside `spawn_blocking` — it attaches to real
/// hardware and reads memory, the same blocking-probe-rs-call posture every
/// hardware-touching handler in `embarch-core` already takes, even though
/// this is a single-user local tool with no concurrent-request pressure to
/// speak of. Redirects back to `/` either way (303, so refreshing the
/// result page never resubmits the form) with a query param the index page
/// reads back to show what happened — there's no session/flash-message
/// infrastructure here, and one query param is simpler than adding any.
async fn enroll_form(Form(body): Form<EnrollFormBody>) -> Redirect {
    let result = tokio::task::spawn_blocking(move || hardware::enroll(&body.role, &body.chip)).await;

    match result {
        Ok(Ok(board)) => Redirect::to(&format!("/?enrolled={}", urlencoding_escape(&board.role))),
        Ok(Err(e)) => Redirect::to(&format!("/?enroll_error={}", urlencoding_escape(&format!("{e:?}")))),
        Err(join_err) => Redirect::to(&format!("/?enroll_error={}", urlencoding_escape(&format!("{join_err:?}")))),
    }
}

/// `Form`'s success path already prevents strays through the enroll route
/// itself, but a query-param value can still land in `href`/text-node
/// output further down — encoded exactly the same way this file already
/// escapes everything else headed into HTML (`html_escape`), just with `%XX`
/// escaping first since this specific string travels through a URL query
/// param, not straight into a tag body.
fn urlencoding_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize, Default)]
struct IndexQuery {
    enrolled: Option<String>,
    enroll_error: Option<String>,
}

async fn index(axum::extract::Query(query): axum::extract::Query<IndexQuery>) -> Html<String> {
    let boards = hardware::list_enrolled().unwrap_or_default();
    let alerts = hardware::recent_alerts(20).unwrap_or_default();
    let attached = hardware::list_attached_probes();

    let mut body = String::new();

    if let Some(role) = &query.enrolled {
        body.push_str(&format!(
            "<p style=\"color:green\">Enrolled role '{}' — see the list below.</p>",
            html_escape(role)
        ));
    }
    if let Some(err) = &query.enroll_error {
        body.push_str(&format!("<p style=\"color:red\">Enrollment failed: {}</p>", html_escape(err)));
    }

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

    // The enroll form is the one thing on this whole page that submits
    // anything — everything else here is a live read of what's already
    // true. Shown regardless of how many probes are attached right now
    // (0/1/2+); the currently-attached list right below it is what tells a
    // human whether they're in a state `enroll` will actually accept, and
    // `enroll` itself still refuses server-side either way (`validate.rs`'s
    // own "exactly one probe" check) — this is guidance, not the real gate.
    body.push_str("<h2>Enroll a board</h2>");
    body.push_str(&format!(
        "<p>Currently attached: {}</p>",
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
    if attached.len() != 1 {
        body.push_str(
            "<p style=\"color:#b8860b\">Enrolling needs exactly one board's probe attached at a time — \
             plug in only the board you mean to enroll, then submit below.</p>",
        );
    }
    body.push_str(
        "<form method=\"post\" action=\"/enroll\">\
           <label>Role: \
             <select name=\"role\">\
               <option value=\"dev-bench\">dev-bench</option>\
               <option value=\"dut\">dut</option>\
             </select>\
           </label> \
           <label>Chip: <input name=\"chip\" list=\"chip-suggestions\" placeholder=\"e.g. nRF54L15\" required></label>\
           <datalist id=\"chip-suggestions\">\
             <option value=\"nRF54L15\">\
             <option value=\"esp32c5\">\
           </datalist> \
           <button type=\"submit\">Enroll attached probe</button>\
         </form>",
    );

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
