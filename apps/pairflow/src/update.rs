//! Check public GitHub Releases and install a newer Windows or macOS build.
//!
//! No token. Unauthenticated `api.github.com` allows about 60 requests an hour.
//! The tray asks before replacing anything. Linux AppImage updates stay manual.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/app4thathq/pairflow/releases/latest";
const MAX_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub version: String,
}

static PENDING: Mutex<Option<ReleaseAsset>> = Mutex::new(None);
static STAGED: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn supported() -> bool {
    platform_asset().is_some()
}

pub fn store_pending(asset: ReleaseAsset) {
    *PENDING.lock().unwrap() = Some(asset);
}

pub fn clear_pending() {
    *PENDING.lock().unwrap() = None;
    *STAGED.lock().unwrap() = None;
}

/// Asset published by CI for this operating system.
pub fn platform_asset() -> Option<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("pairflow-windows-x86_64.exe")
    }
    #[cfg(target_os = "macos")]
    {
        Some("pairflow-macos.dmg")
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        target_os = "macos"
    )))]
    {
        None
    }
}

pub fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let tag = tag.trim().trim_start_matches('v');
    let mut parts = tag.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

pub fn api_url_allowed(url: &str) -> bool {
    url == LATEST
}

/// Hosts a published asset is allowed to come from, including GitHub's
/// download redirect. Anything else is refused before a byte is saved.
pub fn download_url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if rest.contains('@') || rest.contains('\\') {
        return false;
    }
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    if host.is_empty() || host.contains(':') {
        return false;
    }
    match host {
        "github.com" => path.starts_with("app4thathq/pairflow/releases/download/"),
        "release-assets.githubusercontent.com" | "objects.githubusercontent.com" => true,
        _ => false,
    }
}

/// `want` is the asset file name (`pairflow-windows-x86_64.exe` or
/// `pairflow-macos.dmg`). `None` means this platform has no in-app update.
pub fn parse_latest(
    body: &str,
    current: &str,
    want: Option<&str>,
) -> Result<Option<ReleaseAsset>, String> {
    let Some(want) = want else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|err| format!("release JSON: {err}"))?;
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "release has no tag".to_string())?;
    if !is_newer(tag, current) {
        return Ok(None);
    }
    let version = tag.trim().trim_start_matches('v').to_string();
    let assets = value
        .get("assets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{version} has no assets"))?;
    for asset in assets {
        let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if name == want {
            if !download_url_allowed(url) {
                return Err(format!("refusing download URL for {name}"));
            }
            return Ok(Some(ReleaseAsset {
                name: name.to_string(),
                url: url.to_string(),
                version,
            }));
        }
    }
    Err(format!("{version} has no {want}"))
}

pub fn check(current: &str) -> Result<Option<ReleaseAsset>, String> {
    let Some(want) = platform_asset() else {
        return Ok(None);
    };
    if !api_url_allowed(LATEST) {
        return Err("update URL is not the public Pairflow release".into());
    }
    let agent = agent();
    let mut response = agent
        .get(LATEST)
        .header("User-Agent", "pairflow")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|err| err.to_string())?;
    let status = response.status().as_u16();
    if status != 200 {
        let detail = body_text(&mut response);
        return Err(format!("HTTP {status} {detail}"));
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("reading release: {err}"))?;
    parse_latest(&body, current, Some(want))
}

/// Download the pending asset into the temp directory. Does not replace the app.
pub fn stage_pending() -> Result<PathBuf, String> {
    let asset = PENDING
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "no update is ready".to_string())?;
    let path = download_asset(&asset)?;
    *STAGED.lock().unwrap() = Some(path.clone());
    Ok(path)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPlan {
    /// A helper replaces this process after it exits.
    Replace,
    /// The disk image was opened. This process keeps running.
    OpenedDisk,
}

/// Start the platform helper. [`ApplyPlan::Replace`] means the caller must exit
/// so the helper can overwrite the executable.
pub fn launch_staged() -> Result<ApplyPlan, String> {
    let staged = STAGED
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "update was not downloaded".to_string())?;
    #[cfg(target_os = "windows")]
    {
        launch_windows(&staged)
    }
    #[cfg(target_os = "macos")]
    {
        launch_macos(&staged)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = staged;
        Err("in-app install is available on Windows and macOS".into())
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(180)))
        .build()
        .new_agent()
}

fn body_text(response: &mut ureq::http::Response<ureq::Body>) -> String {
    response
        .body_mut()
        .read_to_string()
        .unwrap_or_default()
        .chars()
        .take(180)
        .collect()
}

fn download_asset(asset: &ReleaseAsset) -> Result<PathBuf, String> {
    let version = safe_token(&asset.version);
    let file_name = format!("pairflow-update-{version}-{}", asset.name);
    let dest = std::env::temp_dir().join(file_name);
    let mut current = asset.url.clone();
    let agent = agent();
    for _ in 0..5 {
        if !download_url_allowed(&current) {
            return Err("refusing download from an unexpected host".into());
        }
        let mut response = agent
            .get(&current)
            .header("User-Agent", "pairflow")
            .header("Accept", "application/octet-stream")
            .call()
            .map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let next = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "download redirect had no location".to_string())?
                .to_string();
            current = resolve_redirect(&current, &next)?;
            continue;
        }
        if !(200..300).contains(&status) {
            let detail = body_text(&mut response);
            return Err(format!("download HTTP {status} {detail}"));
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_BYTES)
            .read_to_vec()
            .map_err(|err| format!("reading update: {err}"))?;
        std::fs::write(&dest, bytes).map_err(|err| format!("saving update: {err}"))?;
        return Ok(dest);
    }
    Err("too many download redirects".into())
}

fn resolve_redirect(current: &str, location: &str) -> Result<String, String> {
    if location.starts_with("https://") {
        return Ok(location.to_string());
    }
    if let Some(rest) = location.strip_prefix('/') {
        let host = current
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once('/').map(|(host, _)| host))
            .ok_or_else(|| "cannot resolve download redirect".to_string())?;
        return Ok(format!("https://{host}/{rest}"));
    }
    Err("download redirect is not an https URL".into())
}

fn safe_token(value: &str) -> String {
    let token: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
        .collect();
    if token.is_empty() {
        "update".into()
    } else {
        token
    }
}

pub fn windows_helper_script(pid: u32, staged: &Path, dest: &Path, log: &Path) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\r\n\
         try {{\r\n\
         Wait-Process -Id {pid} -ErrorAction SilentlyContinue\r\n\
         Start-Sleep -Milliseconds 400\r\n\
         Copy-Item -LiteralPath {staged} -Destination {dest} -Force\r\n\
         Start-Process -FilePath {dest}\r\n\
         }} catch {{\r\n\
         Set-Content -LiteralPath {log} -Value $_.Exception.Message\r\n\
         }}\r\n",
        staged = ps_quote(&staged.to_string_lossy()),
        dest = ps_quote(&dest.to_string_lossy()),
        log = ps_quote(&log.to_string_lossy()),
    )
}

pub fn macos_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let app = contents.parent()?;
    if app.extension()? != "app" {
        return None;
    }
    Some(app.to_path_buf())
}

pub fn macos_helper_script(pid: u32, dmg: &Path, app: &Path, mount: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         set -e\n\
         while kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
         hdiutil attach -readonly -nobrowse -mountpoint {mount} {dmg}\n\
         ditto {mount}/Pairflow.app {app}\n\
         hdiutil detach {mount} || true\n\
         rmdir {mount} || true\n\
         xattr -dr com.apple.quarantine {app} || true\n\
         open {app}\n",
        mount = sh_quote(&mount.to_string_lossy()),
        dmg = sh_quote(&dmg.to_string_lossy()),
        app = sh_quote(&app.to_string_lossy()),
    )
}

/// Open a downloaded disk image when Pairflow is not running from `Pairflow.app`.
pub fn macos_open_disk_script(dmg: &Path) -> String {
    format!("#!/bin/sh\nopen {}\n", sh_quote(&dmg.to_string_lossy()))
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "windows")]
fn launch_windows(staged: &Path) -> Result<ApplyPlan, String> {
    let dest = std::env::current_exe().map_err(|err| format!("finding Pairflow: {err}"))?;
    let log = staged.with_extension("log");
    let script_path = staged.with_extension("ps1");
    let script = windows_helper_script(std::process::id(), staged, &dest, &log);
    std::fs::write(&script_path, script).map_err(|err| format!("writing updater: {err}"))?;
    let script_arg = script_path.to_string_lossy().to_string();
    spawn_detached(
        "powershell.exe",
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
            &script_arg,
        ],
    )?;
    Ok(ApplyPlan::Replace)
}

#[cfg(target_os = "macos")]
fn launch_macos(staged: &Path) -> Result<ApplyPlan, String> {
    let exe = std::env::current_exe().map_err(|err| format!("finding Pairflow: {err}"))?;
    let script_path = staged.with_extension("sh");
    let (script, plan) = if let Some(app) = macos_bundle(&exe) {
        let mount = std::env::temp_dir().join(format!("pairflow-mnt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&mount);
        std::fs::create_dir_all(&mount).map_err(|err| format!("preparing mount: {err}"))?;
        (
            macos_helper_script(std::process::id(), staged, &app, &mount),
            ApplyPlan::Replace,
        )
    } else {
        (macos_open_disk_script(staged), ApplyPlan::OpenedDisk)
    };
    std::fs::write(&script_path, script).map_err(|err| format!("writing updater: {err}"))?;
    let script_arg = script_path.to_string_lossy().to_string();
    spawn_detached("/bin/sh", &[&script_arg])?;
    Ok(plan)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn spawn_detached(program: &str, args: &[&str]) -> Result<(), String> {
    let child = std::process::Command::new(program)
        .args(args)
        .spawn()
        .map_err(|err| format!("starting updater: {err}"))?;
    std::mem::forget(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compares_release_tags() {
        assert_eq!(parse_version("v0.2.1"), Some((0, 2, 1)));
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert!(parse_version("v0.2.1-rc1").is_none());
        assert!(is_newer("v0.2.1", "0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.1"));
        assert!(!is_newer("nightly", "0.2.1"));
    }

    #[test]
    fn download_hosts_are_allowlisted() {
        assert!(api_url_allowed(LATEST));
        assert!(!api_url_allowed(
            "https://api.github.com/repos/other/pairflow/releases/latest"
        ));
        assert!(download_url_allowed(
            "https://github.com/app4thathq/pairflow/releases/download/v0.2.1/pairflow-macos.dmg"
        ));
        assert!(download_url_allowed(
            "https://objects.githubusercontent.com/github-production-release-asset-1/x?token=1"
        ));
        assert!(download_url_allowed(
            "https://release-assets.githubusercontent.com/github-production-release-asset/x"
        ));
        assert!(!download_url_allowed(
            "https://github.com/evil/pairflow/releases/download/v0.2.1/pairflow-macos.dmg"
        ));
        assert!(!download_url_allowed(
            "https://github.com.evil/app4thathq/pairflow/releases/download/x"
        ));
        assert!(!download_url_allowed(
            "http://github.com/app4thathq/pairflow/releases/download/v0.2.1/a.exe"
        ));
        assert!(!download_url_allowed(
            "https://user@github.com/app4thathq/pairflow/releases/download/v0.2.1/a.exe"
        ));
    }

    #[test]
    fn latest_json_picks_the_platform_asset() {
        let body = r#"{
            "tag_name": "v0.2.2",
            "assets": [
                {"name": "pairflow-cli.exe", "browser_download_url": "https://github.com/app4thathq/pairflow/releases/download/v0.2.2/pairflow-cli.exe"},
                {"name": "pairflow-windows-x86_64.exe", "browser_download_url": "https://github.com/app4thathq/pairflow/releases/download/v0.2.2/pairflow-windows-x86_64.exe"},
                {"name": "pairflow-macos.dmg", "browser_download_url": "https://github.com/app4thathq/pairflow/releases/download/v0.2.2/pairflow-macos.dmg"}
            ]
        }"#;
        let win = parse_latest(body, "0.2.1", Some("pairflow-windows-x86_64.exe"))
            .unwrap()
            .unwrap();
        assert_eq!(win.version, "0.2.2");
        assert!(win.url.ends_with("pairflow-windows-x86_64.exe"));
        let mac = parse_latest(body, "0.2.1", Some("pairflow-macos.dmg"))
            .unwrap()
            .unwrap();
        assert!(mac.url.ends_with("pairflow-macos.dmg"));
        assert!(parse_latest(body, "0.2.2", Some("pairflow-macos.dmg"))
            .unwrap()
            .is_none());
        assert!(parse_latest(body, "9.0.0", Some("pairflow-macos.dmg"))
            .unwrap()
            .is_none());
        assert!(parse_latest(body, "0.2.1", None).unwrap().is_none());
        let missing = parse_latest(body, "0.2.1", Some("pairflow-nope.bin")).unwrap_err();
        assert!(missing.contains("pairflow-nope.bin"));
    }

    #[test]
    fn rejects_a_newer_release_with_a_foreign_url() {
        let body = r#"{
            "tag_name": "v9.0.0",
            "assets": [
                {"name": "pairflow-macos.dmg", "browser_download_url": "https://evil.example/pairflow-macos.dmg"}
            ]
        }"#;
        let err = parse_latest(body, "0.2.1", Some("pairflow-macos.dmg")).unwrap_err();
        assert!(err.contains("refusing"));
    }

    #[test]
    fn helper_scripts_quote_paths_and_wait() {
        let staged = Path::new("/tmp/pair flow's/update.exe");
        let dest = Path::new(r"C:\Program Files\pairflow-windows-x86_64.exe");
        let script = windows_helper_script(42, staged, dest, Path::new("/tmp/pairflow-update.log"));
        assert!(script.contains("Wait-Process -Id 42"));
        assert!(script.contains("'/tmp/pair flow''s/update.exe'"));
        assert!(script.contains(r"'C:\Program Files\pairflow-windows-x86_64.exe'"));
        assert!(script.contains("Copy-Item"));

        let app = Path::new("/Applications/Pairflow.app");
        assert_eq!(
            macos_bundle(Path::new(
                "/Applications/Pairflow.app/Contents/MacOS/pairflow-gui"
            )),
            Some(app.to_path_buf())
        );
        assert!(macos_bundle(Path::new("/usr/local/bin/pairflow-gui")).is_none());
        let mac = macos_helper_script(
            7,
            Path::new("/tmp/Pair's.dmg"),
            app,
            Path::new("/tmp/pairflow-mnt"),
        );
        assert!(mac.contains("while kill -0 7"));
        assert!(mac.contains("hdiutil attach"));
        assert!(mac.contains("ditto"));
        assert!(mac.contains("xattr -dr com.apple.quarantine"));
        assert!(mac.contains("'/tmp/Pair'\\''s.dmg'"));
        assert!(mac.contains("open '/Applications/Pairflow.app'"));
    }
}
