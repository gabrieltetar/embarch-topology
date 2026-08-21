//! Software topology: where is `embarch-core`, relative to whoever's asking,
//! and how do we know? `embarch-doc/embarch-topology/design.md` §2, §3
//! decisions 3, 4, 11.
//!
//! This is the former `embarch-api`/`embarch-umbrella` `topology.rs` — until
//! 2026-08-21 a deliberately-verbatim mirrored module between those two repos
//! (see `embarch-decision-reversals.md` and this doc's own changelog for the
//! history). It moves here unchanged in its pure-logic core (`candidates`,
//! `classify_status`, `resolve`, `winner`, `detect_wsl2`,
//! `parse_default_gateway` are still exactly what they were, still with no
//! I/O of their own) plus one new thing the mirrored-module split explicitly
//! couldn't have: [`resolve_software_topology`], a single call that also
//! *owns* the I/O (the `reqwest` client, the `/proc/version` read, the `ip
//! route` shell-out) that used to have to live in each consumer's own
//! `env.rs`/`probe.rs`. That was the right shape for two independent copies
//! of the same logic — asking each copy to also own a `reqwest::Client`
//! would have meant embarch-api carrying a *second* one next to the client it
//! already has for talking to Core (this module's own prior doc comment said
//! exactly that). It's the wrong shape now that there's only one
//! implementation: nothing is duplicated by this crate owning its own tiny,
//! short-timeout client, and every consumer gets "give me `base_url`" as one
//! call instead of wiring the pieces together themselves (design.md decision
//! 1's framing).

use std::future::Future;
use std::time::Duration;

/// Core's default port. Overridable everywhere it's used; this is just the
/// value `embarch-core`'s own CLI defaults to.
pub const DEFAULT_CORE_PORT: u16 = 4884;

/// How long [`resolve_software_topology`] waits for one candidate to answer
/// before moving on. Short — the common miss (nothing listening) returns
/// immediately as a connection refusal; this only matters for the rarer case
/// of packets silently dropped, and at most one or two candidates deep
/// (`candidates`' own doc comment).
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// Which *kind* of place Core turned out to be — the only thing worth
/// persisting after detection. Deliberately not the resolved address: under
/// WSL2 that's a gateway IP that changes on every WSL restart, which is
/// exactly the staleness this whole mechanism exists to eliminate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyClass {
    /// Reachable at loopback.
    Local,
    /// Reachable at the WSL2 default gateway, i.e. a Core running natively on
    /// the Windows host of this WSL2 guest.
    WslHost,
    /// A genuinely separate machine, named explicitly by the operator.
    Remote,
}

impl TopologyClass {
    pub fn as_str(self) -> &'static str {
        match self {
            TopologyClass::Local => "local",
            TopologyClass::WslHost => "wsl-host",
            TopologyClass::Remote => "remote",
        }
    }
}

/// One place worth looking for Core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub class: TopologyClass,
    pub base_url: String,
}

/// What a single probe of one candidate found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Core is there. `authorized` distinguishes a `200` from a `401`, which
    /// is a distinction worth keeping all the way to the user: "Core isn't
    /// running" and "Core is running and rejected your token" have nothing to
    /// do with each other, and conflating them sends people to debug the
    /// wrong thing.
    Core { authorized: bool },
    /// Something is listening and speaking HTTP, but it isn't Core — a
    /// different service on the same port, most likely. Not a hit, but worth
    /// surfacing rather than reporting as "nothing there," since the fix
    /// (find out what's on port 4884) is completely different.
    NotCore { status: u16 },
    /// Nothing answered: connection refused, timed out, DNS failure.
    Unreachable,
}

/// One candidate and what probing it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub candidate: Candidate,
    pub outcome: ProbeOutcome,
}

/// Build the ordered list of places to look, cheapest and most likely first.
///
/// Order is load-bearing, not cosmetic. Loopback goes first because it covers
/// three topologies at once — Core native on this Mac/Linux/Windows box, *and*
/// WSL2 in mirrored-networking mode, where loopback already reaches the
/// Windows host. The gateway candidate is what covers WSL2's other (NAT)
/// networking mode. An explicitly configured host goes last: if the operator
/// named a machine, they still shouldn't be reached over the network for a
/// Core sitting on this one.
///
/// Duplicates are dropped, keeping the earliest occurrence — an operator who
/// sets `host = "127.0.0.1"` shouldn't cause the same URL to be probed twice.
pub fn candidates(
    under_wsl2: bool,
    gateway: Option<&str>,
    host: Option<&str>,
    port: u16,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |class, authority: String| {
        let base_url = format!("http://{authority}");
        if !out.iter().any(|c: &Candidate| c.base_url == base_url) {
            out.push(Candidate { class, base_url });
        }
    };

    push(TopologyClass::Local, format!("127.0.0.1:{port}"));

    if under_wsl2 {
        if let Some(gw) = gateway.map(str::trim).filter(|g| !g.is_empty()) {
            push(TopologyClass::WslHost, format!("{gw}:{port}"));
        }
    }

    if let Some(h) = host.map(str::trim).filter(|h| !h.is_empty()) {
        push(TopologyClass::Remote, format!("{h}:{port}"));
    }

    out
}

/// Map an HTTP status from `GET /status` onto what it says about Core.
///
/// `401` counts as finding Core, not as a miss: every one of Core's routes is
/// behind bearer-token auth, so an unauthenticated probe reaching a healthy
/// Core gets exactly this. Anything else that answers is some other service.
pub fn classify_status(status: u16) -> ProbeOutcome {
    match status {
        200 => ProbeOutcome::Core { authorized: true },
        401 => ProbeOutcome::Core { authorized: false },
        other => ProbeOutcome::NotCore { status: other },
    }
}

/// Probe candidates in order, stopping at the first one that is Core.
///
/// Ordered and sequential rather than concurrent, despite "race": ordering is
/// the point (`candidates`' own doc comment), and the common miss — nothing
/// listening — is a connection refusal that returns immediately rather than
/// burning the timeout.
///
/// Returns every attempt made, in order, so a caller can report what it tried
/// and not just what it found. `probe` supplies the actual I/O — kept generic
/// (rather than folded into [`resolve_software_topology`] outright) so tests
/// exercise the ordering/short-circuit logic with no network involved at all.
pub async fn resolve<F, Fut>(candidates: &[Candidate], probe: F) -> Vec<Attempt>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = ProbeOutcome>,
{
    let mut attempts = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let outcome = probe(candidate.base_url.clone()).await;
        let found_core = matches!(outcome, ProbeOutcome::Core { .. });
        attempts.push(Attempt {
            candidate: candidate.clone(),
            outcome,
        });
        if found_core {
            break;
        }
    }
    attempts
}

/// The attempt that found Core, if any.
pub fn winner(attempts: &[Attempt]) -> Option<&Attempt> {
    attempts
        .iter()
        .find(|a| matches!(a.outcome, ProbeOutcome::Core { .. }))
}

/// Are we running inside a WSL2 guest?
///
/// Two independent signals, either of which is enough: the kernel release
/// string (WSL2's kernel is Microsoft-built and says so) and the environment
/// variable WSL itself sets. Neither alone is airtight — `WSL_DISTRO_NAME`
/// can be inherited into a context that isn't really WSL, and a custom kernel
/// might not carry the vendor string — so this takes either.
pub fn detect_wsl2(proc_version: Option<&str>, wsl_distro_env: Option<&str>) -> bool {
    let kernel_says_so = proc_version
        .map(|v| v.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false);
    let env_says_so = wsl_distro_env.map(|v| !v.is_empty()).unwrap_or(false);
    kernel_says_so || env_says_so
}

/// Pull the gateway address out of `ip route show default` output.
///
/// Parses the `via <addr>` form specifically. A default route with no `via`
/// (a point-to-point link) yields nothing, which is correct — there's no
/// host address to talk to in that case.
pub fn parse_default_gateway(ip_route_output: &str) -> Option<String> {
    ip_route_output.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "default" {
            return None;
        }
        let mut fields = fields.skip_while(|f| *f != "via");
        fields.next()?; // the "via" itself
        fields.next().map(str::to_string)
    })
}

// ---- I/O this module now owns outright (design.md §3 decision 2's
// rationale: a compiled-in library call carries none of the "can be down"
// risk that ruled out a standalone service, so there's no reason left to
// keep this out of the crate the way the old mirrored-module split had to).

/// Are we running inside a WSL2 guest, checked against this real machine.
fn under_wsl2_here() -> bool {
    let proc_version = std::fs::read_to_string("/proc/version").ok();
    let wsl_distro_env = std::env::var("WSL_DISTRO_NAME").ok();
    detect_wsl2(proc_version.as_deref(), wsl_distro_env.as_deref())
}

/// This machine's default-route gateway, if it has one — `None` on any
/// platform/failure where `ip route` isn't the right tool (only ever
/// consulted when [`under_wsl2_here`] is already true, so a non-Linux host
/// never pays for the attempt).
fn default_gateway_here() -> Option<String> {
    let output = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_default_gateway(&String::from_utf8_lossy(&output.stdout))
}

async fn probe_core(client: &reqwest::Client, base_url: &str) -> ProbeOutcome {
    match client.get(format!("{base_url}/status")).send().await {
        Ok(resp) => classify_status(resp.status().as_u16()),
        Err(_) => ProbeOutcome::Unreachable,
    }
}

/// The result of one live `resolve_software_topology()` call: what won (if
/// anything), and every attempt tried, for a caller that wants to report
/// *which* candidate answered, not just pass/fail — `doctor`'s existing
/// posture (design.md §2's architecture note on this).
#[derive(Debug, Clone)]
pub struct ResolvedSoftwareTopology {
    pub winner: Option<Candidate>,
    pub attempts: Vec<Attempt>,
}

impl ResolvedSoftwareTopology {
    pub fn base_url(&self) -> Option<&str> {
        self.winner.as_ref().map(|c| c.base_url.as_str())
    }

    pub fn class(&self) -> Option<TopologyClass> {
        self.winner.as_ref().map(|c| c.class)
    }
}

/// Find Core, live, on every call — no cache, no write-ahead file (design.md
/// §3 decision 3). `declared_host` is whatever a consumer's own config
/// declares (`embarch-api`'s `config.toml` `[core].host`, say) — pass `None`
/// when nothing's been declared. `declared_base_url`, when set, is a literal
/// address that always wins outright over auto-detection (matching the
/// existing `base_url` "always wins" precedent) and is probed on its own,
/// skipping candidate generation entirely.
pub async fn resolve_software_topology(
    port: u16,
    declared_host: Option<&str>,
    declared_base_url: Option<&str>,
) -> ResolvedSoftwareTopology {
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let candidates: Vec<Candidate> = if let Some(base_url) = declared_base_url {
        let base_url = base_url.trim_end_matches('/').to_string();
        let class = if base_url.contains("127.0.0.1") || base_url.contains("localhost") {
            TopologyClass::Local
        } else {
            TopologyClass::Remote
        };
        vec![Candidate { class, base_url }]
    } else {
        let under_wsl2 = under_wsl2_here();
        let gateway = if under_wsl2 { default_gateway_here() } else { None };
        candidates(under_wsl2, gateway.as_deref(), declared_host, port)
    };

    let attempts = resolve(&candidates, |url| {
        let client = client.clone();
        async move { probe_core(&client, &url).await }
    })
    .await;
    let winner = winner(&attempts).map(|a| a.candidate.clone());
    ResolvedSoftwareTopology { winner, attempts }
}

/// What bind address Core should be installed with for a given software
/// topology (design.md §2's "bind-address rules"). `embarch-umbrella`'s
/// `setup` is the caller — Core itself just takes whatever `--bind` it's
/// told (`embarch-core/design.md` §3 decision 6) rather than re-deriving
/// this on every start, since the answer is fixed for the life of the
/// installed service (design.md §3 decision 3's "once at process startup for
/// anything that can't change" case).
pub fn recommended_bind_address(class: TopologyClass) -> &'static str {
    match class {
        // Core reachable only from this same machine — the narrow default
        // (`embarch-core/design.md` §3 decision 6's amendment).
        TopologyClass::Local => "127.0.0.1",
        // A WSL2 guest reaches its Windows host over the gateway address,
        // never loopback — Core has to actually listen on every interface
        // for that hop to land.
        TopologyClass::WslHost => "0.0.0.0",
        // A remote Core is, by definition, meant to be reached over the
        // network from wherever `embarch-umbrella setup` is running.
        TopologyClass::Remote => "0.0.0.0",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(c: &[Candidate]) -> Vec<&str> {
        c.iter().map(|c| c.base_url.as_str()).collect()
    }

    #[test]
    fn plain_machine_gets_loopback_only() {
        let c = candidates(false, None, None, 4884);
        assert_eq!(urls(&c), ["http://127.0.0.1:4884"]);
        assert_eq!(c[0].class, TopologyClass::Local);
    }

    #[test]
    fn gateway_is_ignored_when_not_under_wsl2() {
        let c = candidates(false, Some("192.168.1.1"), None, 4884);
        assert_eq!(urls(&c), ["http://127.0.0.1:4884"]);
    }

    #[test]
    fn wsl2_adds_the_gateway_after_loopback() {
        let c = candidates(true, Some("172.22.128.1"), None, 4884);
        assert_eq!(
            urls(&c),
            ["http://127.0.0.1:4884", "http://172.22.128.1:4884"]
        );
        assert_eq!(c[1].class, TopologyClass::WslHost);
    }

    #[test]
    fn wsl2_without_a_gateway_still_tries_loopback() {
        assert_eq!(urls(&candidates(true, None, None, 4884)).len(), 1);
        assert_eq!(urls(&candidates(true, Some("   "), None, 4884)).len(), 1);
    }

    #[test]
    fn explicit_host_goes_last() {
        let c = candidates(true, Some("172.22.128.1"), Some("bench.local"), 4884);
        assert_eq!(
            urls(&c),
            [
                "http://127.0.0.1:4884",
                "http://172.22.128.1:4884",
                "http://bench.local:4884"
            ]
        );
        assert_eq!(c[2].class, TopologyClass::Remote);
    }

    #[test]
    fn duplicate_urls_are_dropped_keeping_the_earliest() {
        let c = candidates(true, Some("127.0.0.1"), Some("127.0.0.1"), 4884);
        assert_eq!(urls(&c), ["http://127.0.0.1:4884"]);
        assert_eq!(c[0].class, TopologyClass::Local);
    }

    #[test]
    fn port_is_honored() {
        let c = candidates(false, None, None, 9999);
        assert_eq!(urls(&c), ["http://127.0.0.1:9999"]);
    }

    #[test]
    fn status_classification() {
        assert_eq!(classify_status(200), ProbeOutcome::Core { authorized: true });
        assert_eq!(
            classify_status(401),
            ProbeOutcome::Core { authorized: false }
        );
        assert_eq!(classify_status(404), ProbeOutcome::NotCore { status: 404 });
        assert_eq!(classify_status(500), ProbeOutcome::NotCore { status: 500 });
    }

    #[tokio::test]
    async fn resolve_stops_at_the_first_core() {
        let c = candidates(true, Some("172.22.128.1"), Some("bench.local"), 4884);
        let attempts = resolve(&c, |url| async move {
            match url.as_str() {
                "http://127.0.0.1:4884" => ProbeOutcome::Unreachable,
                _ => ProbeOutcome::Core { authorized: false },
            }
        })
        .await;

        assert_eq!(attempts.len(), 2, "must not probe past the first hit");
        let w = winner(&attempts).expect("gateway should have won");
        assert_eq!(w.candidate.class, TopologyClass::WslHost);
        assert_eq!(w.outcome, ProbeOutcome::Core { authorized: false });
    }

    #[tokio::test]
    async fn a_non_core_service_does_not_win_but_is_recorded() {
        let c = candidates(true, Some("172.22.128.1"), None, 4884);
        let attempts = resolve(&c, |url| async move {
            match url.as_str() {
                "http://127.0.0.1:4884" => ProbeOutcome::NotCore { status: 404 },
                _ => ProbeOutcome::Core { authorized: true },
            }
        })
        .await;

        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].outcome, ProbeOutcome::NotCore { status: 404 });
        assert_eq!(winner(&attempts).unwrap().candidate.class, TopologyClass::WslHost);
    }

    #[tokio::test]
    async fn nothing_anywhere_reports_every_attempt() {
        let c = candidates(true, Some("172.22.128.1"), Some("bench.local"), 4884);
        let attempts = resolve(&c, |_| async { ProbeOutcome::Unreachable }).await;
        assert_eq!(attempts.len(), 3);
        assert!(winner(&attempts).is_none());
    }

    #[test]
    fn wsl2_detection() {
        let wsl_kernel = "Linux version 6.6.87.2-microsoft-standard-WSL2 (gcc ...)";
        assert!(detect_wsl2(Some(wsl_kernel), None));
        assert!(detect_wsl2(Some("Linux version 6.6.87.2-MICROSOFT"), None));
        assert!(detect_wsl2(None, Some("Ubuntu-24.04")));
        assert!(!detect_wsl2(Some("Linux version 6.8.0-45-generic"), None));
        assert!(!detect_wsl2(None, None));
        assert!(!detect_wsl2(None, Some("")));
    }

    #[test]
    fn gateway_parsing() {
        assert_eq!(
            parse_default_gateway("default via 172.22.128.1 dev eth0 proto kernel"),
            Some("172.22.128.1".to_string())
        );
        assert_eq!(
            parse_default_gateway(
                "10.0.0.0/8 via 10.1.2.3 dev eth1\ndefault via 192.168.0.1 dev eth0\n"
            ),
            Some("192.168.0.1".to_string())
        );
        assert_eq!(parse_default_gateway(""), None);
        assert_eq!(parse_default_gateway("default dev ppp0 scope link"), None);
        assert_eq!(parse_default_gateway("172.22.128.0/20 dev eth0"), None);
    }

    #[test]
    fn recommended_bind_address_matches_decision_6() {
        assert_eq!(recommended_bind_address(TopologyClass::Local), "127.0.0.1");
        assert_eq!(recommended_bind_address(TopologyClass::WslHost), "0.0.0.0");
        assert_eq!(recommended_bind_address(TopologyClass::Remote), "0.0.0.0");
    }
}
