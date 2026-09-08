//! Whether this process is running inside a WSL2 guest — the one fact both
//! [`software`](super::software)'s Core-candidate ordering and
//! [`hardware`](super::hardware)'s dev-bench "not found" diagnosis need
//! (`embarch-topology` decision 27). Split out to its own,
//! unconditionally-compiled module (no `#[cfg(feature = ...)]` on this file
//! at all) rather than living only in `software`, because it has **zero
//! dependencies beyond `std`** — reading `/proc/version` and one env var —
//! and `hardware`-only consumers (`embarch-core`, `src/lib.rs`'s own doc
//! comment) must be able to compute it without pulling in `software`'s
//! `reqwest`/`tokio`, the exact dependency that feature split exists to keep
//! out of their build.
//!
//! `software::detect_wsl2` keeps its own name and signature (an external,
//! already-used API this crate must not break) and just delegates here.

/// Two independent signals, either of which is enough: the kernel release
/// string (WSL2's kernel is Microsoft-built and says so) and the environment
/// variable WSL itself sets. Neither alone is airtight — `WSL_DISTRO_NAME`
/// can be inherited into a context that isn't really WSL, and a custom kernel
/// might not carry the vendor string — so this takes either.
pub fn detect(proc_version: Option<&str>, wsl_distro_env: Option<&str>) -> bool {
    let kernel_says_so = proc_version
        .map(|v| v.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false);
    let env_says_so = wsl_distro_env.map(|v| !v.is_empty()).unwrap_or(false);
    kernel_says_so || env_says_so
}

/// [`detect`] against this real machine — the only I/O in this module, and
/// both reads are local and synchronous (no network, no async runtime
/// needed), which is exactly why a fully-synchronous, `hardware`-only call
/// site (`hardware::port::detect`) can call this directly.
pub fn detect_here() -> bool {
    let proc_version = std::fs::read_to_string("/proc/version").ok();
    let wsl_distro_env = std::env::var("WSL_DISTRO_NAME").ok();
    detect(proc_version.as_deref(), wsl_distro_env.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl2_detection() {
        let wsl_kernel = "Linux version 6.6.87.2-microsoft-standard-WSL2 (gcc ...)";
        assert!(detect(Some(wsl_kernel), None));
        assert!(detect(Some("Linux version 6.6.87.2-MICROSOFT"), None));
        assert!(detect(None, Some("Ubuntu-24.04")));
        assert!(!detect(Some("Linux version 6.8.0-45-generic"), None));
        assert!(!detect(None, None));
        assert!(!detect(None, Some("")));
    }
}
