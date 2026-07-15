//! Singleton / double-launch detection for dynamic-mcp (v1.8.0).
//!
//! ## Why this exists
//!
//! A `http` or `both` instance binds a TCP port. If two instances are started
//! on the **same HTTP endpoint** (host:port/path), one of them cannot use
//! HTTP. Rather than failing silently, we detect the situation at startup and
//! either step aside, take over, or warn the user — depending on which transport
//! is the superset.
//!
//! ## Mechanism
//!
//! Each `http`/`both` instance writes a lock file under
//! `~/.dynamic-mcp/locks/<sha256(endpoint)[..16]>.lock`. The file name is a hash
//! of the full endpoint, so **different endpoints never collide** (they are
//! genuinely independent instances). On startup we atomically try to *create*
//! that file (`O_EXCL`). If it already exists and belongs to a *live* dynamic-mcp
//! process, we compare transport modes and decide what to do.
//!
//! ## Decisions (see [`decide`])
//!
//! * new `http` vs existing `both` / `http` -> **self-terminate** after 8s
//!   (a pure-http instance with no HTTP is useless; the existing instance is reused).
//! * new `both` vs existing `http` (`allow_dual=false`) -> **kill old http**,
//!   stdio starts immediately, HTTP starts 8s later (port is free by then).
//! * new `both` vs existing `http` (`allow_dual=true`) -> **keep stdio only**,
//!   HTTP intentionally off, warn about the waste, do not kill the old http.
//! * new `both` vs existing `both` -> **keep stdio only**, HTTP off, warn, no kill.
//!
//! A pure `stdio` instance never touches the port, so detection is skipped for it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::TransportMode;

/// On-disk record describing a running dynamic-mcp instance that owns an endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceLock {
    /// OS process id of the owner.
    pub pid: u32,
    /// `"http"` or `"both"` — the transport the owner exposes.
    pub transport: String,
    /// Whether the owner declared `--no-evict` (only meaningful for `http`).
    /// A later `both` reads this to decide whether to kill the old http.
    pub allow_dual: bool,
    /// The full HTTP endpoint (host:port/path) the owner bound to.
    pub endpoint: String,
    /// Absolute path of the owner's executable (used to rule out pid reuse).
    pub exe_path: String,
    /// RFC3339 timestamp when the lock was written (diagnostics only).
    pub started_at: String,
}

/// Pure decision produced by [`decide`] from the new transport + the existing lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    /// No relevant conflict.
    None,
    /// New instance is a redundant `http` -> self-terminate after 8s.
    SelfTerminate,
    /// New `both` may evict the old `http` -> kill old, start HTTP after 8s.
    KillOldThenStartHttp,
    /// New `both` keeps its own stdio, leaves HTTP off, does not kill old.
    KeepStdioHttpOff,
}

/// Result of trying to claim the lock file.
#[derive(Debug)]
pub enum AcquireResult {
    /// The file did not exist (or was stale) and we wrote our own lock.
    Acquired,
    /// A live, valid instance already owns the endpoint.
    Conflict(InstanceLock),
}

/// Tells `run_server` how to proceed after the early singleton check.
pub enum StartMode {
    /// No conflict (or we became the primary). Start stdio+http normally.
    Normal,
    /// Visitor: keep stdio only, HTTP intentionally disabled (B2/B3).
    StdioOnly,
    /// B1: stdio starts now, HTTP is delayed 8s (after evicting old http).
    DelayHttpThenNormal,
    /// A1/A3: redundant http — caller shows popup, waits 8s, exits.
    SelfTerminate,
}

/// What [`check_singleton`] returns to `run_server`.
pub struct SingletonResult {
    pub mode: StartMode,
    /// Guard that deletes our own lock file on process exit (if we own one).
    pub guard: Option<LockGuard>,
    /// Warnings/info gathered for the user (layer1 = double-open, layer2 = port).
    pub popup: PopupCollector,
}

// ---------------------------------------------------------------------------
// Lock file paths
// ---------------------------------------------------------------------------

/// Directory holding all instance lock files: `~/.dynamic-mcp/locks/`.
pub fn lock_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dynamic-mcp")
        .join("locks")
}

/// Lock file path for an endpoint: `<lock_dir>/<sha256(endpoint)[..16]>.lock`.
pub fn lock_file_path(endpoint: &str) -> PathBuf {
    lock_dir().join(format!("{}.lock", endpoint_hash(endpoint)))
}

/// Stable, collision-resistant key for an endpoint: first 16 hex chars of SHA-256.
///
/// `DefaultHasher` is NOT used because its seed is randomized per process, so the
/// same endpoint would hash to different file names across runs. SHA-256 is stable.
fn endpoint_hash(endpoint: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(endpoint.as_bytes());
    let out = hasher.finalize();
    let mut s = String::with_capacity(16);
    for b in &out[..8] {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ---------------------------------------------------------------------------
// Pure decision logic
// ---------------------------------------------------------------------------

fn transport_str(t: TransportMode) -> &'static str {
    match t {
        TransportMode::Stdio => "stdio",
        TransportMode::Http => "http",
        TransportMode::Both => "both",
    }
}

/// Decide what a new instance should do given its transport and the existing lock.
///
/// Pure: no I/O, no time. Easy to unit test every branch.
pub fn decide(new_transport: TransportMode, old: &InstanceLock) -> DecisionKind {
    match (new_transport, old.transport.as_str()) {
        // New http is always the redundant party -> it cannot work without HTTP.
        (TransportMode::Http, "both") => DecisionKind::SelfTerminate,
        (TransportMode::Http, "http") => DecisionKind::SelfTerminate,
        // New both vs old http: evict only if the old http did not opt out.
        (TransportMode::Both, "http") if !old.allow_dual => DecisionKind::KillOldThenStartHttp,
        (TransportMode::Both, "http") => DecisionKind::KeepStdioHttpOff,
        // New both vs old both: both is the superset; keep stdio, skip HTTP.
        (TransportMode::Both, "both") => DecisionKind::KeepStdioHttpOff,
        _ => DecisionKind::None,
    }
}

// ---------------------------------------------------------------------------
// Lock acquisition / persistence
// ---------------------------------------------------------------------------

/// Atomically claim the lock, or report a conflict with a live instance.
///
/// Uses `create_new` (the `O_EXCL` equivalent) so two instances starting at the
/// same instant cannot both believe they are primary. If the file already exists
/// we read it; a dead-or-reused pid makes it stale and we overwrite.
pub fn try_acquire_lock(endpoint: &str, lock: &InstanceLock) -> std::io::Result<AcquireResult> {
    let dir = lock_dir();
    let _ = fs::create_dir_all(&dir);
    let path = lock_file_path(endpoint);
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                let content = serde_json::to_string_pretty(lock)?;
                f.write_all(content.as_bytes())?;
                return Ok(AcquireResult::Acquired);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: InstanceLock = match fs::read_to_string(&path)
                    .ok()
                    .and_then(|c| serde_json::from_str(&c).ok())
                {
                    Some(l) => l,
                    None => {
                        // Unreadable or corrupt lock -> treat as stale.
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                };
                if !is_pid_alive(existing.pid) || !is_same_binary(existing.pid, &existing.exe_path)
                {
                    // Stale lock (owner dead or pid reused by another program).
                    let _ = fs::remove_file(&path);
                    continue;
                }
                return Ok(AcquireResult::Conflict(existing));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Overwrite the lock file (used by B1 to become the new primary after eviction).
pub fn write_lock(endpoint: &str, lock: &InstanceLock) -> std::io::Result<()> {
    let dir = lock_dir();
    let _ = fs::create_dir_all(&dir);
    let path = lock_file_path(endpoint);
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    f.write_all(serde_json::to_string_pretty(lock)?.as_bytes())?;
    Ok(())
}

/// RAII guard: deletes *our own* lock file on drop, but only if it still records
/// our pid (so we never delete a lock belonging to a newer primary, e.g. after a
/// B1 takeover the old http must not remove the new both's lock).
pub struct LockGuard {
    path: PathBuf,
    pid: u32,
}

impl LockGuard {
    pub fn new(path: PathBuf, pid: u32) -> Self {
        Self { path, pid }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Ok(content) = fs::read_to_string(&self.path) {
            if let Ok(lock) = serde_json::from_str::<InstanceLock>(&content) {
                if lock.pid == self.pid {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Process detection (cross-platform)
// ---------------------------------------------------------------------------

/// True if `pid` is alive (best-effort; false on any error).
pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0: error-check only, does not actually send a signal.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        unsafe {
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION,
            };
            const STILL_ACTIVE: u32 = 259;
            let h = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            GetExitCodeProcess(h, &mut code);
            windows_sys::Win32::Foundation::CloseHandle(h);
            code == STILL_ACTIVE
        }
    }
}

/// Absolute executable path of `pid`, if we can read it.
fn process_exe_path(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let link = fs::read_link(format!("/proc/{}/exe", pid)).ok()?;
        let canon = fs::canonicalize(&link).unwrap_or(link);
        Some(canon.to_string_lossy().into_owned())
    }
    #[cfg(windows)]
    {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = [0u16; 260];
            let mut size: u32 = buf.len() as u32;
            // BOOL return is ignored; we trust `size` instead.
            QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
            CloseHandle(h);
            if size == 0 {
                return None;
            }
            let s = String::from_utf16_lossy(&buf[..size as usize]);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
    }
}

/// True only if `pid` is alive AND is the same dynamic-mcp executable we expect.
/// This blocks the pid-reuse trap: a dead pid later handed to an unrelated
/// program (e.g. a calculator) would otherwise look "alive".
pub fn is_same_binary(pid: u32, expected_exe: &str) -> bool {
    if !is_pid_alive(pid) {
        return false;
    }
    match process_exe_path(pid) {
        Some(path) => {
            let p = Path::new(&path);
            let e = Path::new(expected_exe);
            p == e || p.file_name() == e.file_name()
        }
        None => false,
    }
}

/// Request termination of `pid`. Unix sends SIGTERM (graceful); Windows uses the
/// forceful `TerminateProcess` (there is no SIGTERM concept on Windows).
pub fn terminate_process(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !h.is_null() {
            TerminateProcess(h, 1);
            CloseHandle(h);
        }
    }
}

/// Unix-only: SIGKILL fallback after SIGTERM did not take effect quickly.
#[cfg(unix)]
pub fn force_kill(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

// ---------------------------------------------------------------------------
// User notifications
// ---------------------------------------------------------------------------

/// Collects layered messages, then emits ONE combined popup (Windows MessageBox /
/// unix stderr banner). Each layer also writes its own `tracing` line so it is
/// captured in logs even when no GUI is present.
pub struct PopupCollector {
    info: Option<String>,
    layer1: Option<String>, // double-open / redundant warning (warn! level)
    layer2: Option<String>, // port conflict (warn! level)
}

impl PopupCollector {
    pub fn new() -> Self {
        Self {
            info: None,
            layer1: None,
            layer2: None,
        }
    }

    /// Informational note (e.g. "both can take over http"). Logged at info level.
    pub fn add_info(&mut self, msg: impl Into<String>) {
        self.info = Some(msg.into());
        tracing::info!("{}", self.info.as_ref().unwrap());
    }

    /// Layer 1: a double-launch / redundant-instance warning.
    pub fn add_double_open(&mut self, msg: impl Into<String>) {
        self.layer1 = Some(msg.into());
        tracing::warn!("{}", self.layer1.as_ref().unwrap());
    }

    /// Layer 2: the HTTP port is (or will be) occupied by another instance.
    pub fn add_port_conflict(&mut self, msg: impl Into<String>) {
        self.layer2 = Some(msg.into());
        tracing::warn!("{}", self.layer2.as_ref().unwrap());
    }

    /// Emit a single combined popup (if anything was collected).
    pub fn emit(&self) {
        let mut lines: Vec<String> = Vec::new();
        if let Some(i) = &self.info {
            lines.push(i.clone());
        }
        if let Some(l) = &self.layer1 {
            lines.push(l.clone());
        }
        if let Some(l) = &self.layer2 {
            lines.push(l.clone());
        }
        if lines.is_empty() {
            return;
        }
        let combined = lines.join("\n\n");
        let level = if self.layer1.is_some() || self.layer2.is_some() {
            "告警"
        } else {
            "提示"
        };
        show_popup(&format!("dynamic-mcp {}", level), &combined);
    }
}

/// Show a user-facing popup.
///
/// * Windows: a real, manually-closable `MessageBoxW` on a background thread
///   (so it never blocks the server).
/// * macOS: `osascript display dialog` (best-effort).
/// * Linux: `notify-send` if available, else nothing (the stderr line below
///   is always printed for headless servers).
/// * All platforms also print to stderr (logs / no-GUI fallback).
pub fn show_popup(title: &str, message: &str) {
    eprintln!("[{}] {}", title, message);

    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING};
        let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let msg_w: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        // `move` the Vecs into the thread so the UTF-16 buffers outlive the call.
        // `HWND` is a type alias in windows-sys (not a tuple struct), so pass a
        // null handle directly rather than constructing it.
        std::thread::spawn(move || unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                msg_w.as_ptr(),
                title_w.as_ptr(),
                MB_ICONWARNING,
            );
        });
    }
    #[cfg(target_os = "macos")]
    {
        // 仅转义 AppleScript 字符串内的 " 与 \（走 exec 不经 shell(命令行解释器)，无需处理 $/反引号）
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        // 裸换行在 AppleScript 字符串字面量里是语法错误 → 用 " & return & " 拼接（修复 mac(苹果系统) 弹窗因含 \n 静默失败）
        let joined = esc(&message)
            .split('\n')
            .collect::<Vec<_>>()
            .join("\" & return & \"");
        let title_esc = esc(&title);
        let script = format!("display dialog \"{}\" with title \"{}\"", joined, title_esc);
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["-u", "critical", title, message])
            .output();
    }
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Run the early singleton check. Returns how `run_server` should start.
///
/// This is called *before* any config loading or upstream connections, so a
/// redundant `http` instance wastes no work before it self-terminates. The
/// endpoint is identified by `host:port/path` (the single `--http-endpoint` CLI arg,
/// form `host:port/path`); two instances on different endpoints are independent.
pub async fn check_singleton(
    transport: TransportMode,
    endpoint: &str,
    display_addr: &str,
    allow_dual: bool,
) -> SingletonResult {
    // Stdio never binds a port -> nothing to detect, no lock to write.
    if matches!(transport, TransportMode::Stdio) {
        return SingletonResult {
            mode: StartMode::Normal,
            guard: None,
            popup: PopupCollector::new(),
        };
    }

    // Full endpoint key: host + port + path. Used as the lock-file name hash so
    // that distinct endpoints never collide (they are genuinely separate instances).
    let my_pid = std::process::id();
    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let my_lock = InstanceLock {
        pid: my_pid,
        transport: transport_str(transport).to_string(),
        allow_dual,
        endpoint: endpoint.to_string(),
        exe_path,
        started_at: chrono::Utc::now().to_rfc3339(),
    };

    match try_acquire_lock(&endpoint, &my_lock) {
        Ok(AcquireResult::Acquired) => SingletonResult {
            mode: StartMode::Normal,
            guard: Some(LockGuard::new(lock_file_path(endpoint), my_pid)),
            popup: PopupCollector::new(),
        },
        Ok(AcquireResult::Conflict(old)) => {
            let decision = decide(transport, &old);
            let mut popup = PopupCollector::new();
            match decision {
                DecisionKind::None => SingletonResult {
                    mode: StartMode::Normal,
                    guard: None,
                    popup,
                },
                DecisionKind::SelfTerminate => {
                    popup.add_double_open(double_open_msg(transport, &old));
                    popup.add_port_conflict(port_conflict_msg(&old, display_addr));
                    SingletonResult {
                        mode: StartMode::SelfTerminate,
                        guard: None,
                        popup,
                    }
                }
                DecisionKind::KillOldThenStartHttp => {
                    // B1: evict the old http now; stdio starts immediately; HTTP
                    // starts 8s later when the port is guaranteed free.
                    terminate_process(old.pid);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    #[cfg(unix)]
                    if is_pid_alive(old.pid) {
                        force_kill(old.pid);
                    }
                    let _ = write_lock(endpoint, &my_lock); // become the new primary
                    popup.add_info(
                        "当前启动的 both 模式可接替已运行的 http 实例：stdio 已立即启动，HTTP 将在 8 秒后接管端口。"
                            .to_string(),
                    );
                    SingletonResult {
                        mode: StartMode::DelayHttpThenNormal,
                        guard: Some(LockGuard::new(lock_file_path(endpoint), my_pid)),
                        popup,
                    }
                }
                DecisionKind::KeepStdioHttpOff => {
                    popup.add_double_open(
                        "检测到同端口已有 dynamic-mcp 实例在运行，存在双开浪费；本次仅启用 stdio，HTTP 功能未启用。"
                            .to_string(),
                    );
                    SingletonResult {
                        mode: StartMode::StdioOnly,
                        guard: None,
                        popup,
                    }
                }
            }
        }
        Err(e) => {
            // Lock dir/IO trouble: never block startup; proceed without detection.
            tracing::warn!("单例检测失败（{}），跳过检测正常启动", e);
            SingletonResult {
                mode: StartMode::Normal,
                guard: None,
                popup: PopupCollector::new(),
            }
        }
    }
}

/// Layer-1 message body (why this launch is redundant).
fn double_open_msg(new: TransportMode, old: &InstanceLock) -> String {
    match (new, old.transport.as_str()) {
        (TransportMode::Http, "both") => {
            "当前已有同端口的 both 模式 dynamic-mcp 在运行，其已包含 http 功能，可直接复用，无需再启动 http 模式。"
                .to_string()
        }
        (TransportMode::Http, "http") => {
            "当前已有同端口的 http 模式 dynamic-mcp 在运行，再开一个 http 属于双开浪费。"
                .to_string()
        }
        _ => "检测到同端口已有 dynamic-mcp 实例，存在双开浪费。".to_string(),
    }
}

/// Layer-2 message body (port occupied + what the user should do).
fn port_conflict_msg(old: &InstanceLock, addr: &str) -> String {
    match old.transport.as_str() {
        "both" => format!(
            "端口 {} 已被已运行的 both 模式 dynamic-mcp 占用。\n建议：① 你不必新开 http —— 已有的 both 模式已包含 http 功能，可直接复用；② 若确实需要单独再跑 http，请在启动命令的 --http-endpoint 与 LLM 的 MCP 配置文件中都改成其他端点（两处必须一致，例如 --http-endpoint 127.0.0.1:9000/dynamic-mcp）。",
            addr
        ),
        "http" => format!(
            "端口 {} 已被已运行的 http 模式 dynamic-mcp 占用。\n建议：若确实需要再开一个 http 实例，请在启动命令的 --http-endpoint 与 LLM 的 MCP 配置文件中都改成其他端点（两处必须一致，例如 --http-endpoint 127.0.0.1:9000/dynamic-mcp）。",
            addr
        ),
        _ => format!("端口 {} 已被另一个 dynamic-mcp 实例占用。", addr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(transport: &str, allow_dual: bool) -> InstanceLock {
        InstanceLock {
            pid: 12345,
            transport: transport.to_string(),
            allow_dual,
            endpoint: "127.0.0.1:8082/dynamic-mcp".to_string(),
            exe_path: "/usr/bin/dmcp".to_string(),
            started_at: "2026-07-14T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn decide_http_vs_both_self_terminates_regardless_of_switch() {
        assert_eq!(
            decide(TransportMode::Http, &lock("both", false)),
            DecisionKind::SelfTerminate
        );
        assert_eq!(
            decide(TransportMode::Http, &lock("both", true)),
            DecisionKind::SelfTerminate
        );
    }

    #[test]
    fn decide_http_vs_http_self_terminates() {
        assert_eq!(
            decide(TransportMode::Http, &lock("http", false)),
            DecisionKind::SelfTerminate
        );
        assert_eq!(
            decide(TransportMode::Http, &lock("http", true)),
            DecisionKind::SelfTerminate
        );
    }

    #[test]
    fn decide_both_vs_http_depends_on_allow_dual() {
        assert_eq!(
            decide(TransportMode::Both, &lock("http", false)),
            DecisionKind::KillOldThenStartHttp
        );
        assert_eq!(
            decide(TransportMode::Both, &lock("http", true)),
            DecisionKind::KeepStdioHttpOff
        );
    }

    #[test]
    fn decide_both_vs_both_keeps_stdio() {
        assert_eq!(
            decide(TransportMode::Both, &lock("both", false)),
            DecisionKind::KeepStdioHttpOff
        );
    }

    #[test]
    fn endpoint_hash_is_deterministic_and_distinct() {
        let a = endpoint_hash("127.0.0.1:8082/dynamic-mcp");
        let b = endpoint_hash("127.0.0.1:8082/dynamic-mcp");
        let c = endpoint_hash("127.0.0.1:8083/dynamic-mcp");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }
}
