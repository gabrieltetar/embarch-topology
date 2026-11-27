//! The machine-wide data directory (`%ProgramData%\embarch` on Windows,
//! `/var/lib/embarch` elsewhere) as one public resolver, plus the two WSL2
//! helpers a Windows-host path needs (`embarch-topology` decision 52).
//!
//! Unconditionally compiled under `software` or `hardware`, with `std` and
//! `anyhow` only, so `embarch-core` (hardware only) and the `software`
//! consumers call the same code. [`TopologyClass`] lives here for that reason
//! and is re-exported by [`software`](crate::software).
//!
//! **The unset policy is one:** `ProgramData` unset on Windows, or a WSL2
//! shell-out that yields nothing usable, is an error naming what was tried.
//! There is no silent literal fallback.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

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

const UNIX_DATA_DIR: &str = "/var/lib/embarch";
const DATA_DIR_NAME: &str = "embarch";

/// The machine-wide data directory as this process reaches it. `Ok(None)` is
/// [`TopologyClass::Remote`] only: no shared filesystem, so no local path (not
/// an error). `Local` is this machine's own directory; `WslHost` is the Windows
/// host's, reached through `/mnt` from inside WSL2, its shell-out cached once
/// per process. Every failure names what was tried; nothing falls back to a
/// guessed `C:\ProgramData`.
pub fn machine_data_dir(class: TopologyClass) -> Result<Option<PathBuf>> {
    match class {
        TopologyClass::Local => local_machine_data_dir().map(Some),
        TopologyClass::WslHost => wsl_host_data_dir().map(Some),
        TopologyClass::Remote => Ok(None),
    }
}

/// This machine's own directory: `%ProgramData%\embarch` on Windows (an error
/// if `ProgramData` is unset or empty), `/var/lib/embarch` elsewhere.
pub fn local_machine_data_dir() -> Result<PathBuf> {
    local_dir_with(cfg!(windows), std::env::var("ProgramData").ok().as_deref())
}

fn local_dir_with(windows: bool, program_data: Option<&str>) -> Result<PathBuf> {
    if !windows {
        return Ok(PathBuf::from(UNIX_DATA_DIR));
    }
    match program_data.map(str::trim).filter(|v| !v.is_empty()) {
        Some(pd) => Ok(PathBuf::from(pd).join(DATA_DIR_NAME)),
        None => bail!("the ProgramData environment variable is not set (or is empty), so the machine data directory cannot be located"),
    }
}

fn wsl_host_data_dir() -> Result<PathBuf> {
    if cfg!(windows) {
        bail!("the WSL host's data directory is only reachable from inside WSL2, not from Windows");
    }
    static CACHE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    CACHE
        .get_or_init(|| compute_wsl_host_data_dir(&run_shellout).map_err(|e| format!("{e:#}")))
        .clone()
        .map_err(|msg| anyhow!(msg))
}

fn compute_wsl_host_data_dir(run: &Runner) -> Result<PathBuf> {
    let program_data = windows_env_var_with("ProgramData", run)
        .context("could not determine Windows %ProgramData% from within WSL2")?;
    let mnt_root = windows_path_to_wsl_with(&program_data, run)
        .with_context(|| format!("could not translate Windows path '{program_data}' to its WSL2 mount form"))?;
    Ok(mnt_root.join(DATA_DIR_NAME))
}

type Runner = dyn Fn(&str, &[&str]) -> Option<String>;

/// The value of a Windows environment variable, read from inside WSL2 by
/// shelling out to `cmd.exe /C echo %NAME%`, then `powershell.exe` if that
/// yields nothing usable. Cached per variable name for the life of the process.
/// An error names both shell-outs. `name` must be letters, digits or `_`.
pub fn windows_env_var(name: &str) -> Result<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Result<String, String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(name) {
        return hit.clone().map_err(|msg| anyhow!(msg));
    }
    let result = windows_env_var_with(name, &run_shellout).map_err(|e| format!("{e:#}"));
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), result.clone());
    result.map_err(|msg| anyhow!(msg))
}

fn windows_env_var_with(name: &str, run: &Runner) -> Result<String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("'{name}' is not a plain Windows environment variable name");
    }
    if let Some(v) = run("cmd.exe", &["/C", "echo", &format!("%{name}%")]) {
        if !v.is_empty() && !v.contains('%') {
            return Ok(v);
        }
    }
    if let Some(v) = run("powershell.exe", &["-NoProfile", "-Command", &format!("$env:{name}")]) {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    bail!("neither cmd.exe nor powershell.exe returned a usable %{name}% value")
}

/// Translates a Windows path (`C:\ProgramData`) to its WSL2 mount form
/// (`/mnt/c/ProgramData`): `wslpath -u` first, since it reflects how this
/// instance actually mounts drives, then a hand parse of the drive letter.
pub fn windows_path_to_wsl(win_path: &str) -> Result<PathBuf> {
    windows_path_to_wsl_with(win_path, &run_shellout)
}

fn windows_path_to_wsl_with(win_path: &str, run: &Runner) -> Result<PathBuf> {
    if let Some(translated) = run("wslpath", &["-u", win_path]) {
        if translated.starts_with('/') {
            return Ok(PathBuf::from(translated));
        }
    }
    hand_parse_windows_path(win_path)
}

fn hand_parse_windows_path(win_path: &str) -> Result<PathBuf> {
    let mut chars = win_path.chars();
    let drive = chars
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .with_context(|| format!("'{win_path}' doesn't start with a drive letter"))?;
    let rest = chars
        .as_str()
        .strip_prefix(':')
        .with_context(|| format!("'{win_path}' is missing the ':' after the drive letter"))?;
    let rest = rest.replace('\\', "/");
    Ok(PathBuf::from(format!("/mnt/{}{}", drive.to_ascii_lowercase(), rest)))
}

fn run_shellout(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing(_: &str, _: &[&str]) -> Option<String> {
        None
    }

    #[test]
    fn remote_has_no_local_path_and_is_not_an_error() {
        assert_eq!(machine_data_dir(TopologyClass::Remote).unwrap(), None);
    }

    #[test]
    fn local_on_unix_is_var_lib_embarch_whatever_programdata_says() {
        assert_eq!(local_dir_with(false, None).unwrap(), PathBuf::from("/var/lib/embarch"));
        assert_eq!(local_dir_with(false, Some(r"D:\PD")).unwrap(), PathBuf::from("/var/lib/embarch"));
    }

    #[test]
    fn local_on_windows_joins_programdata_and_honours_a_relocated_one() {
        assert_eq!(local_dir_with(true, Some("D:/PD")).unwrap(), PathBuf::from("D:/PD").join("embarch"));
    }

    #[test]
    fn local_on_windows_with_programdata_unset_or_empty_is_an_error_naming_it() {
        for v in [None, Some(""), Some("  ")] {
            let msg = local_dir_with(true, v).unwrap_err().to_string();
            assert!(msg.contains("ProgramData"), "{msg}");
        }
    }

    #[test]
    fn a_shellout_that_yields_nothing_is_an_error_naming_both_tried() {
        let msg = windows_env_var_with("ProgramData", &nothing).unwrap_err().to_string();
        assert!(msg.contains("cmd.exe") && msg.contains("powershell.exe") && msg.contains("ProgramData"), "{msg}");
        let msg = format!("{:#}", compute_wsl_host_data_dir(&nothing).unwrap_err());
        assert!(msg.contains("cmd.exe") && msg.contains("powershell.exe"), "{msg}");
    }

    #[test]
    fn cmd_wins_then_powershell_is_the_fallback() {
        let both = |p: &str, _: &[&str]| Some(if p == "cmd.exe" { "C:\\A".to_string() } else { "C:\\B".to_string() });
        assert_eq!(windows_env_var_with("X", &both).unwrap(), "C:\\A");
        // cmd.exe echoes the unexpanded `%X%` for an unset variable.
        let unexpanded = |p: &str, _: &[&str]| Some(if p == "cmd.exe" { "%X%".to_string() } else { "C:\\B".to_string() });
        assert_eq!(windows_env_var_with("X", &unexpanded).unwrap(), "C:\\B");
        let empty_ps = |p: &str, _: &[&str]| Some(if p == "cmd.exe" { "%X%".to_string() } else { String::new() });
        assert!(windows_env_var_with("X", &empty_ps).is_err());
    }

    #[test]
    fn an_env_var_name_that_is_not_plain_is_refused_before_any_shellout() {
        let boom = |_: &str, _: &[&str]| -> Option<String> { panic!("must not run") };
        assert!(windows_env_var_with("A&calc", &boom).is_err());
        assert!(windows_env_var_with("", &boom).is_err());
    }

    #[test]
    fn wslpath_is_preferred_and_the_hand_parse_is_the_fallback() {
        let wslpath = |_: &str, _: &[&str]| Some("/mnt/relocated/PD".to_string());
        assert_eq!(windows_path_to_wsl_with(r"C:\PD", &wslpath).unwrap(), PathBuf::from("/mnt/relocated/PD"));
        let relative = |_: &str, _: &[&str]| Some("not-absolute".to_string());
        assert_eq!(windows_path_to_wsl_with(r"C:\PD", &relative).unwrap(), PathBuf::from("/mnt/c/PD"));
        assert_eq!(windows_path_to_wsl_with(r"C:\PD", &nothing).unwrap(), PathBuf::from("/mnt/c/PD"));
        assert!(windows_path_to_wsl_with("PD", &nothing).is_err());
    }

    #[test]
    fn wsl_host_dir_is_the_translated_programdata_joined_with_embarch() {
        let run = |p: &str, a: &[&str]| match p {
            "cmd.exe" => Some(r"D:\Data\PD".to_string()),
            "wslpath" => Some(format!("/mnt/{}", a[1].replace(":\\", "/").replace('\\', "/").to_lowercase())),
            _ => None,
        };
        assert_eq!(compute_wsl_host_data_dir(&run).unwrap(), PathBuf::from("/mnt/d/data/pd/embarch"));
    }

    #[test]
    fn hand_parse_translates_and_lowercases_the_drive_letter() {
        assert_eq!(hand_parse_windows_path(r"C:\ProgramData").unwrap(), PathBuf::from("/mnt/c/ProgramData"));
        assert_eq!(hand_parse_windows_path(r"D:\Foo\Bar").unwrap(), PathBuf::from("/mnt/d/Foo/Bar"));
        assert!(hand_parse_windows_path(r"C\x").is_err());
    }
}
