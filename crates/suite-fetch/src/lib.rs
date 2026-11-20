//! Downloads: resumable (HTTP `Range`), hashed with SHA-256 while writing, size-checked,
//! retried with exponential backoff on transient failures, and proxied as the environment
//! says (`HTTPS_PROXY`, `NO_PROXY`, and the system proxy on Windows).

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sha2::{Digest, Sha256};
use suite_catalog::Sha256Hex;
use ureq::{
    Agent,
    tls::{RootCerts, TlsConfig, TlsProvider},
};

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{url}: only HTTPS is allowed (plain HTTP only to this computer)")]
    Insecure { url: String },
    #[error("{url}: the server answered {status}")]
    Status { url: String, status: u16 },
    #[error("{url}: {message}")]
    Network { url: String, message: String },
    #[error("{url}: the download stopped early ({got} of {expected} bytes)")]
    Truncated { url: String, got: u64, expected: u64 },
    #[error("{url}: larger than the expected {expected} bytes")]
    TooLarge { url: String, expected: u64 },
    #[error("{url}: checksum mismatch (expected {expected}, got {got})")]
    Checksum { url: String, expected: Sha256Hex, got: Sha256Hex },
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("cancelled")]
    Cancelled,
}

impl FetchError {
    /// Worth another try: network trouble, server errors, an early end.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network { .. } | Self::Truncated { .. } => true,
            Self::Status { status, .. } => *status >= 500 || *status == 408 || *status == 429,
            _ => false,
        }
    }

    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io { path: path.to_owned(), source }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Extra attempts after the first.
    pub retries: u32,
    /// First backoff; doubled after each failure up to `max_backoff`.
    pub backoff: Duration,
    pub max_backoff: Duration,
    pub connect_timeout: Duration,
    /// Give up on a connection that sends nothing for this long.
    pub read_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            retries: 4,
            backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(15),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
        }
    }
}

/// What a download must turn out to be.
#[derive(Debug, Clone)]
pub struct Expected {
    pub size: u64,
    pub sha256: Sha256Hex,
}

/// Download progress in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

/// rustls on Linux; the OS's TLS elsewhere (see the workspace manifest).
#[cfg(target_os = "linux")]
const TLS_PROVIDER: TlsProvider = TlsProvider::Rustls;
#[cfg(not(target_os = "linux"))]
const TLS_PROVIDER: TlsProvider = TlsProvider::NativeTls;

#[derive(Debug, Clone)]
pub struct Fetcher {
    /// Honours the environment's proxy settings.
    agent: Agent,
    /// No proxy: for the loopback interface (tests, local mirrors).
    direct: Agent,
    opts: Options,
    cancel: Arc<AtomicBool>,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new(Options::default())
    }
}

impl Fetcher {
    pub fn new(opts: Options) -> Self {
        let build = |proxy: Option<ureq::Proxy>| -> Agent {
            Agent::config_builder()
                .user_agent(suite_identity::USER_AGENT)
                .http_status_as_error(false)
                .proxy(proxy)
                .timeout_connect(Some(opts.connect_timeout))
                .timeout_recv_response(Some(opts.read_timeout))
                .timeout_per_read(Some(opts.read_timeout))
                .max_redirects(10)
                .tls_config(
                    TlsConfig::builder()
                        .provider(TLS_PROVIDER)
                        .root_certs(RootCerts::PlatformVerifier)
                        .build(),
                )
                .build()
                .into()
        };
        Self { agent: build(ureq::Proxy::try_from_env()), direct: build(None), opts, cancel: Arc::default() }
    }

    /// A flag that stops running downloads with [`FetchError::Cancelled`] when set.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn agent_for(&self, url: &str) -> Result<&Agent, FetchError> {
        suite_catalog::model::validate_url(url).map_err(|_| FetchError::Insecure { url: url.into() })?;
        let loopback = url::Url::parse(url).is_ok_and(|u| match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d == "localhost",
            None => false,
        });
        Ok(if loopback { &self.direct } else { &self.agent })
    }

    fn check_cancel(&self) -> Result<(), FetchError> {
        if self.cancel.load(Ordering::Relaxed) { Err(FetchError::Cancelled) } else { Ok(()) }
    }

    /// Runs `op` with retries and backoff for transient errors.
    fn retrying<T>(&self, url: &str, mut op: impl FnMut() -> Result<T, FetchError>) -> Result<T, FetchError> {
        let mut delay = self.opts.backoff;
        let mut attempt = 0;
        loop {
            self.check_cancel()?;
            match op() {
                Err(e) if e.is_transient() && attempt < self.opts.retries => {
                    attempt += 1;
                    tracing::info!(url, error = %e, attempt, "retrying after {delay:?}");
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(self.opts.max_backoff);
                }
                other => return other,
            }
        }
    }

    fn get(&self, url: &str, range_from: u64) -> Result<ureq::http::Response<ureq::Body>, FetchError> {
        let mut req = self.agent_for(url)?.get(url);
        if range_from > 0 {
            req = req.header("Range", format!("bytes={range_from}-"));
        }
        req.call().map_err(|e| net(url, &e))
    }

    /// GETs a small document (the catalog, API pages), at most `max` bytes.
    pub fn get_bytes(&self, url: &str, max: u64) -> Result<Vec<u8>, FetchError> {
        self.retrying(url, || {
            let resp = self.get(url, 0)?;
            let status = resp.status().as_u16();
            if status != 200 {
                return Err(FetchError::Status { url: url.into(), status });
            }
            let mut out = Vec::new();
            let mut reader = resp.into_body().into_with_config().limit(u64::MAX).reader().take(max + 1);
            reader
                .read_to_end(&mut out)
                .map_err(|e| FetchError::Network { url: url.into(), message: e.to_string() })?;
            if out.len() as u64 > max {
                return Err(FetchError::TooLarge { url: url.into(), expected: max });
            }
            Ok(out)
        })
    }

    /// Streams `url` without storing it and returns its size and SHA-256 (publishing).
    pub fn hash_url(
        &self,
        url: &str,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<(u64, Sha256Hex), FetchError> {
        self.inspect(url, progress).map(|r| (r.size, r.sha256))
    }

    /// Streams `url` without storing it and describes what it served: the final URL after
    /// redirects, the file name (`Content-Disposition`, else the URL's last segment), the
    /// size and the SHA-256. Used to pin vendors' "latest" downloads at publish time.
    pub fn inspect(&self, url: &str, progress: &mut dyn FnMut(Progress)) -> Result<Remote, FetchError> {
        use ureq::ResponseExt as _;
        self.retrying(url, || {
            let resp = self.get(url, 0)?;
            let status = resp.status().as_u16();
            if status != 200 {
                return Err(FetchError::Status { url: url.into(), status });
            }
            let final_url = resp.get_uri().to_string();
            let disposition = resp
                .headers()
                .get("content-disposition")
                .and_then(|v| v.to_str().ok())
                .and_then(disposition_file_name);
            let file_name = disposition.or_else(|| {
                final_url
                    .split(['?', '#'])
                    .next()
                    .and_then(|u| u.rsplit('/').next())
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned)
            });
            let total = resp.body().content_length().unwrap_or(0);
            let mut reader = resp.into_body().into_with_config().limit(u64::MAX).reader();
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 256 * 1024];
            let mut done = 0u64;
            loop {
                self.check_cancel()?;
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| FetchError::Network { url: url.into(), message: e.to_string() })?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                done += n as u64;
                progress(Progress { done, total });
            }
            if total > 0 && done != total {
                return Err(FetchError::Truncated { url: url.into(), got: done, expected: total });
            }
            Ok(Remote {
                final_url,
                file_name,
                size: done,
                sha256: Sha256Hex::from_digest(&hasher.finalize()),
            })
        })
    }

    /// Downloads `url` to `dest`, resuming `dest.part` if an earlier attempt left one, and
    /// checks size and SHA-256. On success `dest` holds exactly the expected bytes; on a
    /// checksum mismatch nothing is kept.
    pub fn download(
        &self,
        url: &str,
        dest: &Path,
        expected: &Expected,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<(), FetchError> {
        let part = part_path(dest);
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| FetchError::io(dir, e))?;
        }
        let mut restarted = false;
        loop {
            let hash = self.retrying(url, || self.attempt(url, &part, expected, progress))?;
            if hash == expected.sha256 {
                std::fs::rename(&part, dest).map_err(|e| FetchError::io(dest, e))?;
                return Ok(());
            }
            // A resumed file may have had a bad start; try once from scratch before blaming
            // the server.
            remove_if_exists(&part)?;
            if restarted {
                return Err(FetchError::Checksum {
                    url: url.into(),
                    expected: expected.sha256.clone(),
                    got: hash,
                });
            }
            tracing::warn!(url, "checksum mismatch; downloading again from the start");
            restarted = true;
        }
    }

    /// One request: continue `part` where it ends and return the SHA-256 of the whole file.
    fn attempt(
        &self,
        url: &str,
        part: &Path,
        expected: &Expected,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Sha256Hex, FetchError> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(part)
            .map_err(|e| FetchError::io(part, e))?;
        let mut have = file.metadata().map_err(|e| FetchError::io(part, e))?.len();
        if have > expected.size {
            file.set_len(0).map_err(|e| FetchError::io(part, e))?;
            have = 0;
        }
        // Hash what is already there, so the digest covers the whole file.
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut left = have;
        while left > 0 {
            let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            let n = file.read(&mut buf[..want]).map_err(|e| FetchError::io(part, e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
        }
        if have == expected.size {
            progress(Progress { done: have, total: expected.size });
            return Ok(Sha256Hex::from_digest(&hasher.finalize()));
        }
        if have > 0 {
            tracing::info!(url, have, "resuming download");
        }

        let resp = self.get(url, have)?;
        let status = resp.status().as_u16();
        let mut done = match status {
            206 if content_range_start(&resp) == Some(have) => have,
            200 | 206 => {
                // No (usable) range support: start over.
                file.set_len(0).map_err(|e| FetchError::io(part, e))?;
                hasher = Sha256::new();
                0
            }
            416 => {
                // Our partial file no longer fits the server's: start over next attempt.
                file.set_len(0).map_err(|e| FetchError::io(part, e))?;
                return Err(FetchError::Truncated { url: url.into(), got: 0, expected: expected.size });
            }
            _ => return Err(FetchError::Status { url: url.into(), status }),
        };
        file.seek(SeekFrom::Start(done)).map_err(|e| FetchError::io(part, e))?;
        progress(Progress { done, total: expected.size });

        let mut reader = resp.into_body().into_with_config().limit(u64::MAX).reader();
        loop {
            self.check_cancel()?;
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    file.sync_data().map_err(|e| FetchError::io(part, e))?;
                    return Err(FetchError::Network { url: url.into(), message: e.to_string() });
                }
            };
            if n == 0 {
                break;
            }
            if done + n as u64 > expected.size {
                drop(file);
                remove_if_exists(part)?;
                return Err(FetchError::TooLarge { url: url.into(), expected: expected.size });
            }
            file.write_all(&buf[..n]).map_err(|e| FetchError::io(part, e))?;
            hasher.update(&buf[..n]);
            done += n as u64;
            progress(Progress { done, total: expected.size });
        }
        file.sync_all().map_err(|e| FetchError::io(part, e))?;
        if done < expected.size {
            return Err(FetchError::Truncated { url: url.into(), got: done, expected: expected.size });
        }
        Ok(Sha256Hex::from_digest(&hasher.finalize()))
    }
}

/// What [`Fetcher::inspect`] found at a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub final_url: String,
    pub file_name: Option<String>,
    pub size: u64,
    pub sha256: Sha256Hex,
}

/// The file name of a `Content-Disposition` header, without any folder part.
pub fn disposition_file_name(header: &str) -> Option<String> {
    let mut plain = None;
    for part in header.split(';').map(str::trim) {
        if let Some(v) = part.strip_prefix("filename*=") {
            // RFC 5987: charset'lang'value; only plain ASCII names are taken as they are.
            if let Some(name) = v.rsplit('\'').next().filter(|n| !n.contains('%')) {
                plain = Some(name.to_owned());
                break;
            }
        } else if let Some(v) = part.strip_prefix("filename=") {
            plain = Some(v.trim_matches('"').to_owned());
        }
    }
    plain
        .and_then(|n| n.rsplit(['/', '\\']).next().map(str::to_owned))
        .filter(|n| !n.is_empty() && n != "." && n != "..")
}

/// `dest` with `.part` appended.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn remove_if_exists(p: &Path) -> Result<(), FetchError> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(FetchError::io(p, e)),
        _ => Ok(()),
    }
}

fn content_range_start(resp: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    let v = resp.headers().get("content-range")?.to_str().ok()?;
    let range = v.strip_prefix("bytes ")?;
    range.split('-').next()?.trim().parse().ok()
}

fn net(url: &str, e: &ureq::Error) -> FetchError {
    FetchError::Network { url: url.into(), message: e.to_string() }
}

/// SHA-256 of a local file.
pub fn sha256_file(path: &Path) -> std::io::Result<Sha256Hex> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Sha256Hex::from_digest(&hasher.finalize()))
}

/// SHA-256 of bytes.
pub fn sha256(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_digest(&Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use suite_testkit::{Fault, Server};

    use super::*;

    fn fast() -> Fetcher {
        Fetcher::new(Options { backoff: Duration::from_millis(5), ..Options::default() })
    }

    fn payload(n: usize) -> Vec<u8> {
        (0..n).map(|i| u8::try_from(i * 7 % 251).unwrap()).collect()
    }

    #[test]
    fn downloads_and_verifies() {
        let server = Server::start();
        let data = payload(300_000);
        server.put("/a.bin", data.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        let mut last = None;
        let exp = Expected { size: data.len() as u64, sha256: sha256(&data) };
        fast().download(&server.url("/a.bin"), &dest, &exp, &mut |p| last = Some(p)).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(last, Some(Progress { done: 300_000, total: 300_000 }));
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn resumes_a_truncated_download() {
        let server = Server::start();
        let data = payload(500_000);
        server.put("/big", data.clone());
        // The first response dies after 200 kB; the retry must ask for the rest only.
        server.fault("/big", Fault::TruncateAfter(200_000), 1);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("big");
        let exp = Expected { size: data.len() as u64, sha256: sha256(&data) };
        fast().download(&server.url("/big"), &dest, &exp, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let ranges = server.ranges_requested("/big");
        assert_eq!(ranges, [None, Some(200_000)]);
    }

    #[test]
    fn resumes_a_partial_file_left_by_a_previous_run() {
        let server = Server::start();
        let data = payload(100_000);
        server.put("/p", data.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("p");
        std::fs::write(part_path(&dest), &data[..60_000]).unwrap();
        let exp = Expected { size: data.len() as u64, sha256: sha256(&data) };
        fast().download(&server.url("/p"), &dest, &exp, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(server.ranges_requested("/p"), [Some(60_000)]);
    }

    #[test]
    fn corrupt_partial_file_is_redone_from_scratch() {
        let server = Server::start();
        let data = payload(50_000);
        server.put("/c", data.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("c");
        std::fs::write(part_path(&dest), vec![0xEE; 10_000]).unwrap();
        let exp = Expected { size: data.len() as u64, sha256: sha256(&data) };
        fast().download(&server.url("/c"), &dest, &exp, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(server.ranges_requested("/c"), [Some(10_000), None]);
    }

    #[test]
    fn checksum_mismatch_keeps_nothing() {
        let server = Server::start();
        server.put("/x", b"evil bytes".to_vec());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x");
        let exp = Expected { size: 10, sha256: sha256(b"good bytes") };
        let err = fast().download(&server.url("/x"), &dest, &exp, &mut |_| {}).unwrap_err();
        assert!(matches!(err, FetchError::Checksum { .. }), "{err}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn oversize_is_refused() {
        let server = Server::start();
        server.put("/o", payload(5000));
        let dir = tempfile::tempdir().unwrap();
        let exp = Expected { size: 1000, sha256: sha256(b"") };
        let err = fast().download(&server.url("/o"), &dir.path().join("o"), &exp, &mut |_| {}).unwrap_err();
        assert!(matches!(err, FetchError::TooLarge { .. }), "{err}");
    }

    #[test]
    fn retries_server_errors_then_gives_up_on_404() {
        let server = Server::start();
        server.put("/flaky", b"ok".to_vec());
        server.fault("/flaky", Fault::Status(503), 2);
        assert_eq!(fast().get_bytes(&server.url("/flaky"), 100).unwrap(), b"ok");
        let err = fast().get_bytes(&server.url("/missing"), 100).unwrap_err();
        assert!(matches!(err, FetchError::Status { status: 404, .. }));
        assert_eq!(server.hits("/missing"), 1);
    }

    #[test]
    fn get_bytes_has_a_limit() {
        let server = Server::start();
        server.put("/doc", payload(2000));
        assert!(matches!(fast().get_bytes(&server.url("/doc"), 1000), Err(FetchError::TooLarge { .. })));
    }

    #[test]
    fn refuses_plain_http_to_other_hosts() {
        let err = fast().get_bytes("http://example.com/catalog.json", 10).unwrap_err();
        assert!(matches!(err, FetchError::Insecure { .. }));
    }

    #[test]
    fn hash_url_streams() {
        let server = Server::start();
        let data = payload(70_000);
        server.put("/h", data.clone());
        let (size, hash) = fast().hash_url(&server.url("/h"), &mut |_| {}).unwrap();
        assert_eq!(size, 70_000);
        assert_eq!(hash, sha256(&data));
    }

    #[test]
    fn inspect_reports_the_file_name() {
        assert_eq!(
            disposition_file_name(
                "attachment; filename=\"zynk-ui-linux-x86_64-091ee20dff0c.AppImage\"; filename*=UTF-8''zynk-ui-linux-x86_64-091ee20dff0c.AppImage"
            )
            .as_deref(),
            Some("zynk-ui-linux-x86_64-091ee20dff0c.AppImage")
        );
        assert_eq!(disposition_file_name("attachment; filename=\"../../x.bin\"").as_deref(), Some("x.bin"));
        assert_eq!(disposition_file_name("inline"), None);
        let server = Server::start();
        server.put("/dl/app-1.2.3.tgz", b"abc".to_vec());
        let r = fast().inspect(&server.url("/dl/app-1.2.3.tgz?x=1"), &mut |_| {}).unwrap();
        assert_eq!((r.file_name.as_deref(), r.size), (Some("app-1.2.3.tgz"), 3));
        assert_eq!(r.sha256, sha256(b"abc"));
    }

    #[test]
    fn cancel_stops() {
        let server = Server::start();
        server.put("/z", payload(10));
        let f = fast();
        f.cancel_flag().store(true, Ordering::Relaxed);
        assert!(matches!(f.get_bytes(&server.url("/z"), 100), Err(FetchError::Cancelled)));
    }
}
