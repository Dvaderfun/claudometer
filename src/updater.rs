//! Self-update from GitHub Releases — passive, transparent, user-initiated.
//!
//! Check: `releases/latest` once per day (and once at launch), in a worker
//! thread; failures are silent and drafts/prereleases are skipped. No config,
//! no nag, no toast — the only surfaces are the settings About card and a dot
//! on the flyout gear.
//!
//! Install (only when the user clicks): download the exe asset next to the
//! current exe, verify it (PE magic, VERSIONINFO == release tag, SHA256 via
//! certutil when the release ships a `.sha256` asset), then the rename swap —
//! Windows lets a *running* exe be renamed, so: exe → .old, new → exe, spawn
//! `--swap-wait`, quit. The new instance waits for the single-instance mutex,
//! then deletes the `.old`. Any failure rolls back and falls back to opening
//! the release page.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

// Owner/name also live in Cargo.toml `repository` — keep in sync.
const API_LATEST: &str = "https://api.github.com/repos/Dvaderfun/claudometer/releases/latest";
pub const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const UA: &str = concat!("claudometer/", env!("CARGO_PKG_VERSION"));
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone)]
pub struct Release {
    pub tag: String,
    version: (u16, u16, u16),
    exe_url: String,
    sha_url: Option<String>,
    pub page_url: String,
}

#[derive(Clone, Default)]
pub enum Status {
    #[default]
    UpToDate,
    Available(Release),
    Installing,
    /// transient failure caption; release page still reachable via the button
    Failed(String, Option<String>),
}

static STATUS: Mutex<Status> = Mutex::new(Status::UpToDate);
static LAST_CHECK: Mutex<Option<Instant>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

pub fn has_update() -> bool {
    matches!(*STATUS.lock().unwrap(), Status::Available(_))
}

fn set_status(s: Status) {
    *STATUS.lock().unwrap() = s;
    let h = crate::MAIN_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut _), crate::WM_UPDATE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Called on every poll tick — actually checks at most once per CHECK_EVERY.
pub fn maybe_check() {
    {
        let last = LAST_CHECK.lock().unwrap();
        if last.map(|t| t.elapsed() < CHECK_EVERY).unwrap_or(false) {
            return;
        }
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let found = check_inner();
        *LAST_CHECK.lock().unwrap() = Some(Instant::now());
        BUSY.store(false, Ordering::SeqCst);
        match found {
            Ok(Some(rel)) => set_status(Status::Available(rel)),
            // silent: up to date, no releases yet, network down, rate limited…
            Ok(None) => {
                // …but never downgrade an already-known update to UpToDate
                if !has_update() {
                    set_status(Status::UpToDate);
                }
            }
            Err(()) => {}
        }
    });
}

// ---------- check ----------

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: Option<String>,
    html_url: Option<String>,
    draft: Option<bool>,
    prerelease: Option<bool>,
    assets: Option<Vec<ApiAsset>>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: Option<String>,
    browser_download_url: Option<String>,
}

fn agent(timeout_secs: u64) -> Option<ureq::Agent> {
    let tls = native_tls::TlsConnector::new().ok()?;
    Some(
        ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(tls))
            .timeout(Duration::from_secs(timeout_secs))
            .build(),
    )
}

fn check_inner() -> Result<Option<Release>, ()> {
    let agent = agent(10).ok_or(())?;
    let resp = agent
        .get(API_LATEST)
        .set("User-Agent", UA)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|_| ())?;
    let body = resp.into_string().map_err(|_| ())?;
    let rel: ApiRelease = serde_json::from_str(&body).map_err(|_| ())?;
    Ok(pick_release(&rel, parse_ver(env!("CARGO_PKG_VERSION")).ok_or(())?))
}

/// Newer, non-draft, non-prerelease release with a claudometer.exe asset.
fn pick_release(rel: &ApiRelease, current: (u16, u16, u16)) -> Option<Release> {
    if rel.draft == Some(true) || rel.prerelease == Some(true) {
        return None;
    }
    let tag = rel.tag_name.clone()?;
    let version = parse_ver(&tag)?;
    if version <= current {
        return None;
    }
    let assets = rel.assets.as_deref().unwrap_or(&[]);
    let url_of = |n: &str| {
        assets
            .iter()
            .find(|a| a.name.as_deref() == Some(n))
            .and_then(|a| a.browser_download_url.clone())
    };
    Some(Release {
        version,
        exe_url: url_of("claudometer.exe")?,
        sha_url: url_of("claudometer.exe.sha256"),
        page_url: rel.html_url.clone().unwrap_or_else(|| REPO_URL.to_string()),
        tag,
    })
}

/// "v0.5.0" / "0.5.0" → (0, 5, 0); anything fancier (rc-suffix etc.) → None.
fn parse_ver(s: &str) -> Option<(u16, u16, u16)> {
    let mut it = s.trim().trim_start_matches('v').split('.');
    let out = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    if it.next().is_some() {
        return None;
    }
    Some(out)
}

// ---------- install ----------

/// User clicked Install. Runs in a worker thread; progress lands in `Status`
/// and repaints via WM_UPDATE. On success the thread posts WM_UPDATE with
/// wparam 1 — the UI thread quits and the freshly spawned exe takes over.
pub fn install() {
    let Status::Available(rel) = status() else { return };
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    set_status(Status::Installing);
    std::thread::spawn(move || {
        let r = install_inner(&rel);
        BUSY.store(false, Ordering::SeqCst);
        match r {
            Ok(()) => {
                let h = crate::MAIN_HWND.load(Ordering::SeqCst);
                unsafe {
                    let _ = PostMessageW(HWND(h as *mut _), crate::WM_UPDATE, WPARAM(1), LPARAM(0));
                }
            }
            Err(msg) => {
                // best help on failure: hand the user the release page
                open_url(&rel.page_url);
                set_status(Status::Failed(msg, Some(rel.page_url.clone())));
            }
        }
    });
}

fn install_inner(rel: &Release) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|_| "can't locate exe")?;
    let dir = exe.parent().ok_or("can't locate exe folder")?;
    let new = dir.join("claudometer.new.exe");
    let old = dir.join("claudometer.old.exe");

    let agent = agent(180).ok_or("TLS init failed")?;
    download(&agent, &rel.exe_url, &new).map_err(|e| format!("download failed: {e}"))?;

    let cleanup_new = |msg: &str| -> String {
        let _ = std::fs::remove_file(&new);
        msg.to_string()
    };

    // integrity: right shape, right version, right hash
    let len = std::fs::metadata(&new).map(|m| m.len()).unwrap_or(0);
    if len < 100_000 {
        return Err(cleanup_new("download truncated"));
    }
    let mut magic = [0u8; 2];
    std::fs::File::open(&new)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map_err(|_| cleanup_new("downloaded file unreadable"))?;
    if &magic != b"MZ" {
        return Err(cleanup_new("downloaded file is not an exe"));
    }
    if file_version(&new) != Some(rel.version) {
        return Err(cleanup_new("downloaded exe version mismatch"));
    }
    if let Some(sha_url) = &rel.sha_url {
        let expected = agent
            .get(sha_url)
            .set("User-Agent", UA)
            .call()
            .map_err(|_| cleanup_new("hash download failed"))?
            .into_string()
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_lowercase))
            .ok_or_else(|| cleanup_new("hash asset unreadable"))?;
        let actual = sha256_of(&new).ok_or_else(|| cleanup_new("hashing failed"))?;
        if actual != expected {
            return Err(cleanup_new("SHA256 mismatch"));
        }
    }

    swap_files(&exe, &new, &old).map_err(|_| cleanup_new("couldn't replace exe (folder read-only?)"))?;

    // hand over: new instance waits for our mutex, then cleans up the .old
    std::process::Command::new(&exe)
        .arg("--swap-wait")
        .spawn()
        .map_err(|_| "relaunch failed — restart Claudometer manually".to_string())?;
    Ok(())
}

fn download(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<(), String> {
    let resp = agent
        .get(url)
        .set("User-Agent", UA)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            _ => "network error".to_string(),
        })?;
    let mut file = std::fs::File::create(dest).map_err(|_| "folder not writable")?;
    // 100 MB cap — a claudometer.exe orders of magnitude bigger is not ours
    std::io::copy(&mut resp.into_reader().take(100 * 1024 * 1024), &mut file)
        .map_err(|_| "write failed")?;
    Ok(())
}

/// exe → .old, new → exe; rollback if the second rename fails.
/// (Windows allows renaming a running exe — deleting/overwriting it, no.)
fn swap_files(exe: &Path, new: &Path, old: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(old); // stale leftover from a crashed update
    std::fs::rename(exe, old)?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(old, exe);
        return Err(e);
    }
    Ok(())
}

/// Post-update startup: drop the previous exe once its process is gone.
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let old = dir.join("claudometer.old.exe");
    if old.exists() {
        for _ in 0..10 {
            if std::fs::remove_file(&old).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

pub fn open_url(url: &str) {
    unsafe {
        let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        windows::Win32::UI::Shell::ShellExecuteW(
            HWND::default(),
            windows::core::w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

// ---------- verification helpers ----------

/// (major, minor, patch) from the exe's VERSIONINFO resource.
fn file_version(path: &Path) -> Option<(u16, u16, u16)> {
    unsafe {
        let wide: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), 0, size, buf.as_mut_ptr() as *mut _).ok()?;
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        if !VerQueryValueW(
            buf.as_ptr() as *const _,
            windows::core::w!("\\"),
            &mut ptr,
            &mut len,
        )
        .as_bool()
            || ptr.is_null()
            || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = &*(ptr as *const VS_FIXEDFILEINFO);
        if info.dwSignature != 0xFEEF_04BD {
            return None;
        }
        Some((
            (info.dwFileVersionMS >> 16) as u16,
            (info.dwFileVersionMS & 0xFFFF) as u16,
            (info.dwFileVersionLS >> 16) as u16,
        ))
    }
}

/// SHA256 via certutil (ships with Windows) — no crypto dependency.
fn sha256_of(path: &Path) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("certutil")
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_certutil(&String::from_utf8_lossy(&out.stdout))
}

/// certutil output: header line, hex line (spaces possible on old builds),
/// trailer. The hash is the only line that strips down to 64 hex chars.
fn parse_certutil(s: &str) -> Option<String> {
    s.lines().find_map(|l| {
        let h: String = l.chars().filter(|c| !c.is_whitespace()).collect();
        (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_ver("v0.5.0"), Some((0, 5, 0)));
        assert_eq!(parse_ver("0.5.0"), Some((0, 5, 0)));
        assert_eq!(parse_ver("1.12.3"), Some((1, 12, 3)));
        assert_eq!(parse_ver("v0.5"), None);
        assert_eq!(parse_ver("v0.5.0.1"), None);
        assert_eq!(parse_ver("v0.5.0-rc1"), None);
    }

    #[test]
    fn version_ordering() {
        assert!(parse_ver("v0.5.0") > parse_ver("v0.4.9"));
        assert!(parse_ver("v0.10.0") > parse_ver("v0.9.9"));
        assert!(parse_ver("v1.0.0") > parse_ver("v0.99.99"));
    }

    #[test]
    fn certutil_parse_plain_and_spaced() {
        let plain = "SHA256 hash of x.exe:\r\nd2b2f2a1c3e4556677889900aabbccddeeff00112233445566778899aabbccdd\r\nCertUtil: -hashfile command completed successfully.\r\n";
        assert_eq!(
            parse_certutil(plain).as_deref(),
            Some("d2b2f2a1c3e4556677889900aabbccddeeff00112233445566778899aabbccdd")
        );
        let spaced = "SHA256 hash of x.exe:\r\nd2 b2 f2 a1 c3 e4 55 66 77 88 99 00 aa bb cc dd ee ff 00 11 22 33 44 55 66 77 88 99 aa bb cc dd\r\ndone\r\n";
        assert_eq!(
            parse_certutil(spaced).as_deref(),
            Some("d2b2f2a1c3e4556677889900aabbccddeeff00112233445566778899aabbccdd")
        );
        assert_eq!(parse_certutil("no hash here"), None);
    }

    #[test]
    fn release_picking() {
        let rel = ApiRelease {
            tag_name: Some("v9.9.9".into()),
            html_url: Some("https://github.com/x/y/releases/tag/v9.9.9".into()),
            draft: Some(false),
            prerelease: Some(false),
            assets: Some(vec![
                ApiAsset {
                    name: Some("claudometer.exe".into()),
                    browser_download_url: Some("https://dl/claudometer.exe".into()),
                },
                ApiAsset {
                    name: Some("claudometer.exe.sha256".into()),
                    browser_download_url: Some("https://dl/claudometer.exe.sha256".into()),
                },
            ]),
        };
        let picked = pick_release(&rel, (0, 4, 0)).expect("newer release picked");
        assert_eq!(picked.tag, "v9.9.9");
        assert!(picked.sha_url.is_some());

        // same or older version → no update
        assert!(pick_release(&rel, (9, 9, 9)).is_none());
        assert!(pick_release(&rel, (10, 0, 0)).is_none());

        // prerelease → skipped
        let pre = ApiRelease { prerelease: Some(true), ..rel };
        assert!(pick_release(&pre, (0, 4, 0)).is_none());
    }

    #[test]
    fn swap_dance_and_rollback() {
        let dir = std::env::temp_dir().join(format!("cm-swap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("app.exe");
        let new = dir.join("app.new.exe");
        let old = dir.join("app.old.exe");

        // happy path: old content preserved as .old, new becomes exe
        std::fs::write(&exe, b"v1").unwrap();
        std::fs::write(&new, b"v2").unwrap();
        swap_files(&exe, &new, &old).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"v2");
        assert_eq!(std::fs::read(&old).unwrap(), b"v1");
        assert!(!new.exists());

        // failure path: missing new → exe restored
        let _ = std::fs::remove_file(&old);
        assert!(swap_files(&exe, &new, &old).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"v2");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
