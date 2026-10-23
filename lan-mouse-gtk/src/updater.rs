//! In-app updates: feed parsing, version comparison, download with
//! SHA-256 verification and the platform specific "install" step.
//!
//! This module intentionally does not depend on GTK so it can be unit tested
//! (and type-checked for other targets) on its own. All functions here block;
//! callers run them on a worker thread.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Where the update feed (`latest.json`) lives.
pub const DEFAULT_FEED_URL: &str =
    "https://github.com/baldmaxim/lan-mouse/releases/latest/download/latest.json";
/// Environment variable overriding [`DEFAULT_FEED_URL`] (for testing).
pub const FEED_URL_ENV: &str = "LAN_MOUSE_UPDATE_URL";

const USER_AGENT: &str = "lan-mouse-updater";
/// Upper bound for a downloaded update package.
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const FEED_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Error, Debug)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Http(String),
    #[error("invalid update feed: {0}")]
    Feed(String),
    #[error("file error: {0}")]
    Io(#[from] io::Error),
    #[error("checksum mismatch, the downloaded file was discarded")]
    ChecksumMismatch,
    #[error("{0}")]
    Install(String),
}

impl From<ureq::Error> for UpdateError {
    fn from(e: ureq::Error) -> Self {
        UpdateError::Http(e.to_string())
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub version: String,
    pub notes_url: String,
    #[serde(default)]
    pub assets: HashMap<String, Asset>,
}

/// A newer release that is available for this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: String,
    pub notes_url: String,
    /// `None` if there is no package for this platform (e.g. Linux): the
    /// user can then only be pointed at the release page.
    pub asset: Option<Asset>,
}

/// Command that applies a downloaded update (see [`prepare_install`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInstall {
    program: PathBuf,
    args: Vec<String>,
}

/// URL of the update feed, honoring [`FEED_URL_ENV`].
pub fn feed_url() -> String {
    std::env::var(FEED_URL_ENV)
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_FEED_URL.to_string())
}

/// Key of this platform in `latest.json` `assets`, `None` if in-app
/// installation is not supported here.
pub fn platform_asset_key() -> Option<&'static str> {
    asset_key_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn asset_key_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("macos", "aarch64") => Some("macos-aarch64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        _ => None,
    }
}

fn parse_version(v: &str) -> Result<Version, UpdateError> {
    Version::parse(v.trim().trim_start_matches('v'))
        .map_err(|e| UpdateError::Feed(format!("bad version \"{v}\": {e}")))
}

/// `true` if `latest` is strictly newer than `current`.
pub fn is_newer(current: &str, latest: &str) -> Result<bool, UpdateError> {
    Ok(parse_version(latest)? > parse_version(current)?)
}

pub fn parse_feed(json: &str) -> Result<Feed, UpdateError> {
    serde_json::from_str(json).map_err(|e| UpdateError::Feed(e.to_string()))
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Evaluates a parsed feed: `Some` if it announces a newer version than
/// `current`. `feed_url` is used to refuse plain-http assets from an https
/// feed.
pub fn evaluate_feed(
    feed: Feed,
    current: &str,
    feed_url: &str,
    asset_key: Option<&str>,
) -> Result<Option<UpdateInfo>, UpdateError> {
    if !is_newer(current, &feed.version)? {
        return Ok(None);
    }
    let asset = match asset_key.and_then(|k| feed.assets.get(k)) {
        Some(asset) => {
            if !is_sha256_hex(&asset.sha256) {
                return Err(UpdateError::Feed("invalid sha256 in feed".into()));
            }
            if feed_url.starts_with("https://") && !asset.url.starts_with("https://") {
                return Err(UpdateError::Feed("asset url is not https".into()));
            }
            Some(asset.clone())
        }
        None => None,
    };
    Ok(Some(UpdateInfo {
        version: feed.version.trim().trim_start_matches('v').to_string(),
        notes_url: feed.notes_url,
        asset,
    }))
}

fn agent(recv_body: Option<Duration>) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_recv_body(recv_body)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Fetches the feed and returns the update if a newer version exists.
pub fn check(current: &str) -> Result<Option<UpdateInfo>, UpdateError> {
    let url = feed_url();
    log::debug!("checking for updates at {url}");
    let mut response = agent(Some(FEED_TIMEOUT)).get(&url).call()?;
    let body = response.body_mut().read_to_string()?;
    let feed = parse_feed(&body)?;
    let info = evaluate_feed(feed, current, &url, platform_asset_key())?;
    match &info {
        Some(info) => log::info!("update available: {} (current {current})", info.version),
        None => log::info!("no update available (current {current})"),
    }
    Ok(info)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file as lowercase hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Fails unless the SHA-256 of `path` equals `expected`; a file that does
/// not match is deleted.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), UpdateError> {
    let actual = sha256_file(path)?;
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        let _ = fs::remove_file(path);
        Err(UpdateError::ChecksumMismatch)
    }
}

/// Directory for downloaded packages.
pub fn update_dir() -> PathBuf {
    std::env::temp_dir().join("lan-mouse-update")
}

/// Safe local file name for a package URL (last path segment).
fn asset_file_name(url: &str) -> String {
    let last = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let valid = !last.is_empty()
        && last != "."
        && last != ".."
        && last
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if valid {
        last.to_string()
    } else {
        "update.bin".to_string()
    }
}

/// Downloads `url` to `dest` (via a `.part` file), reporting
/// `(bytes_done, total_if_known)`.
fn download_to(
    url: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<(), UpdateError> {
    let mut response = agent(Some(DOWNLOAD_TIMEOUT)).get(url).call()?;
    let total = response.body().content_length();
    if total.is_some_and(|t| t > MAX_DOWNLOAD_BYTES) {
        return Err(UpdateError::Http("update package is too large".into()));
    }
    let part = dest.with_extension("part");
    let result = (|| -> Result<(), UpdateError> {
        let mut file = File::create(&part)?;
        let mut reader = response.body_mut().as_reader();
        let mut buf = [0u8; 64 * 1024];
        let mut done = 0u64;
        progress(0, total);
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| UpdateError::Http(e.to_string()))?;
            if n == 0 {
                break;
            }
            done += n as u64;
            if done > MAX_DOWNLOAD_BYTES {
                return Err(UpdateError::Http("update package is too large".into()));
            }
            file.write_all(&buf[..n])?;
            progress(done, total);
        }
        file.flush()?;
        if total.is_some_and(|t| t != done) {
            return Err(UpdateError::Http("download was truncated".into()));
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            fs::rename(&part, dest)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Downloads the package into `dir` (emptied first) and verifies its
/// SHA-256. A file that fails verification is deleted.
pub fn download_and_verify(
    asset: &Asset,
    dir: &Path,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, UpdateError> {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    fs::create_dir_all(dir)?;
    let dest = dir.join(asset_file_name(&asset.url));
    log::info!("downloading update from {}", asset.url);
    download_to(&asset.url, &dest, progress)?;
    verify_sha256(&dest, &asset.sha256)?;
    log::info!("update package verified: {}", dest.display());
    Ok(dest)
}

/// Prepares applying the downloaded package `pkg`. Blocking (unpacks on
/// macOS); nothing is changed for the running app yet.
pub fn prepare_install(pkg: &Path) -> Result<PendingInstall, UpdateError> {
    #[cfg(target_os = "windows")]
    {
        Ok(windows_install(pkg))
    }
    #[cfg(target_os = "macos")]
    {
        macos_install(pkg)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = pkg;
        Err(UpdateError::Install(
            "in-app installation is not supported on this platform".into(),
        ))
    }
}

/// Starts the prepared install detached from this process. The caller must
/// quit the application afterwards.
pub fn launch_install(pending: &PendingInstall) -> Result<(), UpdateError> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(&pending.program);
    cmd.args(&pending.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // own process group: survives when we (and our group) go away
        cmd.process_group(0);
    }
    log::info!("starting installer: {:?}", pending);
    cmd.spawn()
        .map(drop)
        .map_err(|e| UpdateError::Install(format!("could not start the installer: {e}")))
}

#[cfg(any(target_os = "windows", test))]
fn windows_install_args() -> Vec<String> {
    ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]
        .map(String::from)
        .to_vec()
}

#[cfg(target_os = "windows")]
fn windows_install(installer: &Path) -> PendingInstall {
    PendingInstall {
        program: installer.to_path_buf(),
        args: windows_install_args(),
    }
}

/// `X.app` for `…/X.app/Contents/MacOS/<exe>`.
#[cfg(any(target_os = "macos", test))]
fn bundle_path_from_exe(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    let is_app = bundle.extension().is_some_and(|e| e == "app");
    let in_macos_dir = exe.parent()?.file_name()? == "MacOS";
    (is_app && in_macos_dir).then(|| bundle.to_path_buf())
}

/// Single-quotes `s` for /bin/sh.
#[cfg(any(target_os = "macos", test))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Shell script that replaces `old_app` with `new_app` once process `pid`
/// has exited, rolls back on failure and starts the app again.
#[cfg(any(target_os = "macos", test))]
fn macos_update_script(pid: u32, old_app: &str, new_app: &str) -> String {
    let old = sh_quote(old_app);
    let new = sh_quote(new_app);
    format!(
        r#"#!/bin/sh
exec >"$(dirname "$0")/update.log" 2>&1
set -x
PID={pid}
OLD={old}
NEW={new}
i=0
while kill -0 "$PID" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 60 ]; then
    echo "old process did not exit, giving up"
    exit 1
  fi
  sleep 0.5
done
rm -rf "$OLD.old"
if mv "$OLD" "$OLD.old"; then
  if mv "$NEW" "$OLD"; then
    xattr -dr com.apple.quarantine "$OLD"
    rm -rf "$OLD.old"
  else
    rm -rf "$OLD"
    mv "$OLD.old" "$OLD"
  fi
fi
open "$OLD"
"#
    )
}

#[cfg(target_os = "macos")]
fn macos_install(zip: &Path) -> Result<PendingInstall, UpdateError> {
    use std::process::Command;

    let exe = std::env::current_exe()?;
    let old_app = bundle_path_from_exe(&exe).ok_or_else(|| {
        UpdateError::Install(format!(
            "Lan Mouse is not running from an .app bundle ({}), cannot update in place",
            exe.display()
        ))
    })?;
    let parent = old_app.parent().ok_or_else(|| {
        UpdateError::Install(format!("invalid bundle path {}", old_app.display()))
    })?;
    let probe = parent.join(format!(".lan-mouse-write-test-{}", std::process::id()));
    match File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
        }
        Err(e) => {
            return Err(UpdateError::Install(format!(
                "no write access to {} ({e}); move Lan Mouse to /Applications or ~/Applications",
                parent.display()
            )));
        }
    }

    let work_dir = zip
        .parent()
        .ok_or_else(|| UpdateError::Install("invalid package path".into()))?;
    let unpacked = work_dir.join("unpacked");
    let _ = fs::remove_dir_all(&unpacked);
    fs::create_dir_all(&unpacked)?;
    let status = Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(zip)
        .arg(&unpacked)
        .status()
        .map_err(|e| UpdateError::Install(format!("could not run ditto: {e}")))?;
    if !status.success() {
        return Err(UpdateError::Install(format!("ditto failed ({status})")));
    }
    let new_app = unpacked.join("Lan Mouse.app");
    if !new_app
        .join("Contents")
        .join("MacOS")
        .join("lan-mouse")
        .is_file()
    {
        return Err(UpdateError::Install(
            "the update package does not contain Lan Mouse.app".into(),
        ));
    }

    let script = work_dir.join("install.sh");
    let text = macos_update_script(
        std::process::id(),
        &old_app.to_string_lossy(),
        &new_app.to_string_lossy(),
    );
    fs::write(&script, text)?;
    Ok(PendingInstall {
        program: PathBuf::from("/bin/sh"),
        args: vec![script.to_string_lossy().into_owned()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        thread,
    };

    const SHA_A: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn feed_json(version: &str) -> String {
        format!(
            r#"{{"version":"{version}","notes_url":"https://example.com/notes",
            "assets":{{
              "windows-x86_64":{{"url":"https://example.com/Setup-{version}.exe","sha256":"{SHA_A}"}},
              "macos-aarch64":{{"url":"https://example.com/arm.zip","sha256":"{SHA_A}"}}
            }}}}"#
        )
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lan-mouse-updater-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_feed() {
        let feed = parse_feed(&feed_json("0.13.0")).unwrap();
        assert_eq!(feed.version, "0.13.0");
        assert_eq!(feed.assets.len(), 2);
        assert_eq!(
            feed.assets["macos-aarch64"].url,
            "https://example.com/arm.zip"
        );
        assert!(parse_feed("not json").is_err());
        assert!(parse_feed(r#"{"version":"1.0.0"}"#).is_err());
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.12.0", "0.12.1").unwrap());
        assert!(is_newer("0.12.0", "0.13.0").unwrap());
        assert!(is_newer("0.9.0", "0.10.0").unwrap());
        assert!(is_newer("0.12.0", "v1.0.0").unwrap());
        assert!(!is_newer("0.12.0", "0.12.0").unwrap());
        assert!(!is_newer("0.12.1", "0.12.0").unwrap());
        assert!(!is_newer("0.12.0", "0.12.0-rc.1").unwrap());
        assert!(is_newer("0.12.0", "garbage").is_err());
    }

    #[test]
    fn selects_asset_key() {
        assert_eq!(asset_key_for("windows", "x86_64"), Some("windows-x86_64"));
        assert_eq!(asset_key_for("macos", "aarch64"), Some("macos-aarch64"));
        assert_eq!(asset_key_for("macos", "x86_64"), Some("macos-x86_64"));
        assert_eq!(asset_key_for("linux", "x86_64"), None);
        assert_eq!(asset_key_for("windows", "aarch64"), None);
    }

    #[test]
    fn evaluates_feed() {
        let url = "https://example.com/latest.json";
        let feed = parse_feed(&feed_json("0.13.0")).unwrap();

        let info = evaluate_feed(feed.clone(), "0.12.0", url, Some("windows-x86_64"))
            .unwrap()
            .unwrap();
        assert_eq!(info.version, "0.13.0");
        assert_eq!(
            info.asset.unwrap().url,
            "https://example.com/Setup-0.13.0.exe"
        );

        // no package for the platform: still announced, without asset
        let info = evaluate_feed(feed.clone(), "0.12.0", url, None)
            .unwrap()
            .unwrap();
        assert!(info.asset.is_none());
        let info = evaluate_feed(feed.clone(), "0.12.0", url, Some("macos-x86_64"))
            .unwrap()
            .unwrap();
        assert!(info.asset.is_none());

        // up to date
        assert!(
            evaluate_feed(feed.clone(), "0.13.0", url, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_bad_assets() {
        let mut feed = parse_feed(&feed_json("0.13.0")).unwrap();
        feed.assets.get_mut("macos-aarch64").unwrap().sha256 = "abc".into();
        assert!(
            evaluate_feed(
                feed,
                "0.12.0",
                "https://example.com/latest.json",
                Some("macos-aarch64")
            )
            .is_err()
        );

        let mut feed = parse_feed(&feed_json("0.13.0")).unwrap();
        feed.assets.get_mut("macos-aarch64").unwrap().url = "http://example.com/a.zip".into();
        // https feed must not point at plain http
        assert!(
            evaluate_feed(
                feed.clone(),
                "0.12.0",
                "https://example.com/latest.json",
                Some("macos-aarch64")
            )
            .is_err()
        );
        // an http test feed may
        assert!(
            evaluate_feed(
                feed,
                "0.12.0",
                "http://127.0.0.1:8000/latest.json",
                Some("macos-aarch64")
            )
            .is_ok()
        );
    }

    #[test]
    fn verifies_sha256() {
        let dir = temp_dir("sha");
        let file = dir.join("abc.bin");
        fs::write(&file, b"abc").unwrap();
        assert_eq!(sha256_file(&file).unwrap(), SHA_A);
        verify_sha256(&file, SHA_A).unwrap();
        verify_sha256(&file, &SHA_A.to_uppercase()).unwrap();
        assert!(file.exists());

        let err = verify_sha256(&file, &"0".repeat(64)).unwrap_err();
        assert!(matches!(err, UpdateError::ChecksumMismatch));
        assert!(!file.exists(), "file must be deleted on mismatch");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizes_asset_file_name() {
        assert_eq!(
            asset_file_name("https://x/y/LanMouse-Setup-0.13.0.exe"),
            "LanMouse-Setup-0.13.0.exe"
        );
        assert_eq!(asset_file_name("https://x/a.zip?token=1"), "a.zip");
        assert_eq!(asset_file_name("https://x/"), "update.bin");
        assert_eq!(asset_file_name("https://x/a b;c"), "update.bin");
        assert_eq!(asset_file_name("https://x/.."), "update.bin");
    }

    #[test]
    fn finds_bundle_path() {
        assert_eq!(
            bundle_path_from_exe(Path::new(
                "/Applications/Lan Mouse.app/Contents/MacOS/lan-mouse"
            )),
            Some(PathBuf::from("/Applications/Lan Mouse.app"))
        );
        assert_eq!(
            bundle_path_from_exe(Path::new("/usr/local/bin/lan-mouse")),
            None
        );
        assert_eq!(
            bundle_path_from_exe(Path::new("/x/y.app/Contents/Other/lan-mouse")),
            None
        );
    }

    #[test]
    fn builds_macos_script() {
        let script = macos_update_script(
            4242,
            "/Applications/Lan Mouse.app",
            "/tmp/u's/Lan Mouse.app",
        );
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("PID=4242\n"));
        assert!(script.contains("OLD='/Applications/Lan Mouse.app'\n"));
        assert!(script.contains("NEW='/tmp/u'\\''s/Lan Mouse.app'\n"));
        assert!(script.contains("kill -0 \"$PID\""));
        assert!(script.contains("mv \"$OLD\" \"$OLD.old\""));
        assert!(script.contains("mv \"$NEW\" \"$OLD\""));
        assert!(script.contains("xattr -dr com.apple.quarantine \"$OLD\""));
        assert!(script.contains("mv \"$OLD.old\" \"$OLD\""));
        assert!(script.trim_end().ends_with("open \"$OLD\""));
    }

    #[test]
    fn windows_installer_args() {
        assert_eq!(
            windows_install_args(),
            ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]
        );
    }

    /// Minimal HTTP server answering one request with `body`.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok() && line != "\r\n" && !line.is_empty() {
                line.clear();
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        format!("http://{addr}/pkg.bin")
    }

    #[test]
    fn downloads_and_verifies() {
        let dir = temp_dir("download");
        let url = serve_once(b"abc".to_vec());
        let asset = Asset {
            url,
            sha256: SHA_A.to_string(),
        };
        let mut last = (0, None);
        let path = download_and_verify(&asset, &dir, |done, total| last = (done, total)).unwrap();
        assert_eq!(path, dir.join("pkg.bin"));
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        assert_eq!(last, (3, Some(3)));
        assert!(!dir.join("pkg.part").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_with_wrong_hash_is_deleted() {
        let dir = temp_dir("download-bad");
        let asset = Asset {
            url: serve_once(b"abc".to_vec()),
            sha256: "0".repeat(64),
        };
        let err = download_and_verify(&asset, &dir, |_, _| {}).unwrap_err();
        assert!(matches!(err, UpdateError::ChecksumMismatch));
        assert!(!dir.join("pkg.bin").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
