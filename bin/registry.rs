//! Whether the OS has an `embarch-core` service registered, and the command
//! line it was registered with (decisions 28 and 23, amended 2026-11-24).
//!
//! A registered service is the identity store's only writer, running or not,
//! so the CLI's store-writing subcommands ask this before they probe anything.
//! Every read is read-only and every failure — no `reg.exe`, no `systemctl`,
//! no `sc.exe`, no such service, output that does not parse — is "not
//! registered", the answer that leaves local-bootstrap working.

use embarch_topology::software::DEFAULT_CORE_PORT;

/// The Windows service key and the systemd unit `embarch-core install` makes.
const WINDOWS_SERVICE: &str = "com.embarch.core";
#[cfg(not(windows))]
const SYSTEMD_UNIT: &str = "embarch-core";

/// One registration, read from the command line the OS stores for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Where it was read, for the refusal: `Windows service com.embarch.core`.
    pub source: String,
    /// `--bind`, when the command line names one; Core's default otherwise.
    pub bind: Option<String>,
    /// `--port`, when the command line names one.
    pub port: Option<u16>,
    /// The command line carries `--service`: the store is Core's alone.
    pub service_mode: bool,
}

impl Registration {
    /// Core's address as the registration names it, for the route in a refusal.
    /// A wildcard bind is reached on loopback.
    pub fn base_url(&self) -> String {
        let host = match self.bind.as_deref() {
            None | Some("0.0.0.0") | Some("::") | Some("[::]") => "127.0.0.1",
            Some(h) => h,
        };
        format!("http://{host}:{}", self.port.unwrap_or(DEFAULT_CORE_PORT))
    }
}

/// The seam the guard asks. A test passes a fake; `main` passes [`SystemRegistry`].
pub trait ServiceRegistry {
    /// Every Core service registered on this machine; empty when none is.
    fn registrations(&self) -> Vec<Registration>;
}

/// The real OS: the registry on Windows; `systemctl` on Linux, and under WSL2
/// also the Windows side through `sc.exe qc`, as `embarch deploy-core` reads it.
pub struct SystemRegistry;

impl ServiceRegistry for SystemRegistry {
    fn registrations(&self) -> Vec<Registration> {
        let mut found = Vec::new();
        #[cfg(windows)]
        found.extend(windows_registry());
        #[cfg(not(windows))]
        {
            found.extend(systemd_unit());
            let proc_version = std::fs::read_to_string("/proc/version").ok();
            let distro = std::env::var("WSL_DISTRO_NAME").ok();
            if embarch_topology::software::detect_wsl2(proc_version.as_deref(), distro.as_deref()) {
                found.extend(windows_sc_qc());
            }
        }
        found
    }
}

/// Reads `ImagePath` under `HKLM\SYSTEM\CurrentControlSet\Services\com.embarch.core`.
/// Through `reg.exe`, which ships with Windows, so this needs no registry crate.
#[cfg(windows)]
fn windows_registry() -> Option<Registration> {
    let key = format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{WINDOWS_SERVICE}");
    let out = std::process::Command::new("reg")
        .args(["query", &key, "/v", "ImagePath"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("ImagePath"))?;
    Some(parse_command_line(format!("Windows service {WINDOWS_SERVICE}"), line))
}

/// Reads the Windows service from a WSL2 guest, through `sc.exe qc`.
#[cfg(not(windows))]
fn windows_sc_qc() -> Option<Registration> {
    let out = std::process::Command::new("sc.exe").args(["qc", WINDOWS_SERVICE]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("BINARY_PATH_NAME"))?;
    Some(parse_command_line(format!("Windows service {WINDOWS_SERVICE} (sc.exe qc)"), line))
}

/// Reads the `embarch-core` systemd unit: loaded means registered, started or not.
#[cfg(not(windows))]
fn systemd_unit() -> Option<Registration> {
    let out = std::process::Command::new("systemctl")
        .args(["show", SYSTEMD_UNIT, "--property=LoadState,ExecStart"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if !text.lines().any(|l| l.trim() == "LoadState=loaded") {
        return None;
    }
    let exec = text.lines().find(|l| l.starts_with("ExecStart=")).unwrap_or("");
    Some(parse_command_line(format!("systemd unit {SYSTEMD_UNIT}"), exec))
}

/// Pulls `--bind`, `--port` and `--service` out of one stored command line.
/// Both `--bind x` and `--bind=x`; a flag with nothing after it is absent.
/// systemd's `argv[]=... ;` punctuation is stripped from each word.
pub fn parse_command_line(source: String, line: &str) -> Registration {
    let clean = |w: &str| w.trim_matches(|c| c == '"' || c == ';').to_string();
    let words: Vec<String> = line.split_whitespace().map(clean).collect();
    let value_of = |flag: &str| -> Option<String> {
        let eq = format!("{flag}=");
        words.iter().enumerate().find_map(|(i, w)| {
            if let Some(v) = w.strip_prefix(&eq) {
                return Some(v.to_string());
            }
            (w == flag).then(|| words.get(i + 1).cloned()).flatten()
        })
        .filter(|v| !v.is_empty() && !v.starts_with("--"))
    };
    Registration {
        source,
        bind: value_of("--bind"),
        port: value_of("--port").and_then(|p| p.parse().ok()),
        service_mode: words.iter().any(|w| w == "--service"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bind_port_and_service_from_each_spelling() {
        let r = parse_command_line(
            "x".into(),
            r#"    BINARY_PATH_NAME   : "C:\core\embarch-core.exe" run --bind 0.0.0.0 --port=4999 --service"#,
        );
        assert_eq!(r.bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(r.port, Some(4999));
        assert!(r.service_mode);
        assert_eq!(r.base_url(), "http://127.0.0.1:4999");
    }

    #[test]
    fn reads_systemd_argv() {
        let r = parse_command_line(
            "x".into(),
            "ExecStart={ path=/usr/bin/embarch-core ; argv[]=/usr/bin/embarch-core run --bind 10.1.2.3 ; ignore_errors=no }",
        );
        assert_eq!(r.bind.as_deref(), Some("10.1.2.3"));
        assert_eq!(r.port, None);
        assert!(!r.service_mode);
        assert_eq!(r.base_url(), format!("http://10.1.2.3:{DEFAULT_CORE_PORT}"));
    }

    #[test]
    fn a_flag_with_nothing_after_it_is_absent() {
        let r = parse_command_line("x".into(), "core run --bind");
        assert_eq!(r.bind, None);
        let r = parse_command_line("x".into(), "core run --bind --service");
        assert_eq!(r.bind, None);
        assert!(r.service_mode);
    }
}
