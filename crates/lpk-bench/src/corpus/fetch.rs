//! Downloading: the [`Fetcher`] abstraction, the HTTPS implementation, and the verified cache.

use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::lock::LockEntry;
use super::registry::Profile;

/// Why opening or reading a URL failed.
#[derive(Debug)]
pub enum FetchError {
    /// Worth retrying (429, 5xx, timeout, connection or mid-body failure).
    Transient {
        message: String,
        retry_after: Option<Duration>,
    },
    /// Not worth retrying (404, 403, bad URL, ...).
    Permanent(String),
}

/// Source of bytes for a URL. Tests implement this over memory; production uses [`HttpFetcher`].
pub trait Fetcher: fmt::Debug {
    /// Open a stream of the response body. Called again from scratch on retry.
    fn open(&self, url: &str) -> Result<Box<dyn Read>, FetchError>;
}

/// In-process HTTPS client (no external curl).
#[derive(Debug)]
pub struct HttpFetcher {
    client: reqwest::blocking::Client,
}

/// User-Agent sent on every request.
pub fn user_agent() -> String {
    format!(
        "lpk-bench/{} (+https://github.com/Charon89/litepack)",
        env!("CARGO_PKG_VERSION")
    )
}

impl HttpFetcher {
    pub fn new() -> anyhow::Result<HttpFetcher> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(30))
            // Total per-request limit (blocking reqwest has no per-read timeout); generous
            // because artifacts can be large. A timeout is retried like any transient error.
            .timeout(Duration::from_secs(2 * 3600))
            .build()?;
        Ok(HttpFetcher { client })
    }
}

impl Fetcher for HttpFetcher {
    fn open(&self, url: &str) -> Result<Box<dyn Read>, FetchError> {
        let resp = self.client.get(url).send().map_err(|e| {
            if e.is_builder() || e.is_redirect() {
                FetchError::Permanent(format!("{e}"))
            } else {
                FetchError::Transient {
                    message: format!("{e}"),
                    retry_after: None,
                }
            }
        })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(Box::new(resp));
        }
        if status.as_u16() == 429 || status.is_server_error() {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(FetchError::Transient {
                message: format!("HTTP {status}"),
                retry_after,
            });
        }
        Err(FetchError::Permanent(format!("HTTP {status}")))
    }
}

/// Retry behaviour: exponential backoff from `base_delay`, capped at `max_delay`.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    /// Delay before attempt `attempt + 1` (`attempt` counts from 1).
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let backoff = self
            .base_delay
            .saturating_mul(1u32 << (attempt - 1).min(16))
            .min(self.max_delay);
        retry_after.map_or(backoff, |r| r.min(self.max_delay.max(backoff)))
    }
}

/// A verified file in the cache.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub path: PathBuf,
    pub bytes: u64,
    pub blake3: String,
}

/// What the downloaded bytes are checked against.
#[derive(Debug, Clone, Copy)]
pub enum Expect<'a> {
    /// Normal build: the lock entry. Cache is reused only if its re-computed hash matches.
    Pinned(&'a LockEntry),
    /// `--update-lock`: nothing to check against; always download, never trust the cache.
    Unpinned,
}

/// Download failure. Only [`DownloadError::Fetch`] may be tolerated for optional sources.
#[derive(Debug)]
pub enum DownloadError {
    Fetch(String),
    Mismatch(String),
    Io(String),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::Fetch(m) | DownloadError::Mismatch(m) | DownloadError::Io(m) => {
                f.write_str(m)
            }
        }
    }
}

impl std::error::Error for DownloadError {}

/// Hash a file with BLAKE3: `(bytes, lowercase hex)`.
pub fn hash_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let n = std::io::copy(&mut file, &mut hasher)?;
    Ok((n, hasher.finalize().to_hex().to_string()))
}

/// Fetches artifacts into a cache directory, one request at a time.
#[derive(Debug)]
pub struct Downloader<'a> {
    fetcher: &'a dyn Fetcher,
    cache_dir: PathBuf,
    retry: RetryPolicy,
    profile: Profile,
}

impl<'a> Downloader<'a> {
    pub fn new(
        fetcher: &'a dyn Fetcher,
        cache_dir: PathBuf,
        retry: RetryPolicy,
        profile: Profile,
    ) -> Self {
        Downloader {
            fetcher,
            cache_dir,
            retry,
            profile,
        }
    }

    /// Cache location for `(source, url)`.
    pub fn cache_path(&self, source: &str, url: &str) -> PathBuf {
        let h = blake3::hash(url.as_bytes()).to_hex();
        self.cache_dir
            .join(format!("{source}-{}", &h.as_str()[..12]))
    }

    /// Return a verified local copy of `url`.
    pub fn obtain(
        &self,
        source: &str,
        url: &str,
        expect: Expect<'_>,
    ) -> Result<Artifact, DownloadError> {
        let io = |e: std::io::Error| DownloadError::Io(format!("source `{source}`: {e}"));
        std::fs::create_dir_all(&self.cache_dir).map_err(io)?;
        let path = self.cache_path(source, url);

        if let Expect::Pinned(pin) = expect {
            if path.is_file() {
                let (bytes, blake3) = hash_file(&path).map_err(io)?;
                if bytes == pin.bytes && blake3 == pin.blake3 {
                    return Ok(Artifact {
                        path,
                        bytes,
                        blake3,
                    });
                }
                eprintln!("  cache for `{source}` does not match the lock; fetching again");
            }
        }

        let part = path.with_extension("part");
        let (bytes, blake3) = self.download(source, url, &part)?;
        if let Expect::Pinned(pin) = expect {
            if bytes != pin.bytes || blake3 != pin.blake3 {
                let _ = std::fs::remove_file(&part);
                return Err(DownloadError::Mismatch(format!(
                    "source `{source}`: {url} does not match bench/corpus.lock \
                     (locked {} bytes blake3 {}, got {bytes} bytes blake3 {blake3}). \
                     If upstream legitimately changed, review it and re-pin with \
                     `lpk-bench corpus build --profile {} --only <class> --update-lock`.",
                    pin.bytes,
                    pin.blake3,
                    self.profile.name()
                )));
            }
        }
        std::fs::rename(&part, &path).map_err(io)?;
        Ok(Artifact {
            path,
            bytes,
            blake3,
        })
    }

    fn download(
        &self,
        source: &str,
        url: &str,
        part: &Path,
    ) -> Result<(u64, String), DownloadError> {
        let mut attempt = 1;
        loop {
            match self.try_once(url, part) {
                Ok(r) => return Ok(r),
                Err(AttemptError::Local(e)) => {
                    return Err(DownloadError::Io(format!("source `{source}`: {e}")))
                }
                Err(AttemptError::Fetch(FetchError::Permanent(m))) => {
                    return Err(DownloadError::Fetch(format!(
                        "source `{source}`: {url}: {m}"
                    )))
                }
                Err(AttemptError::Fetch(FetchError::Transient {
                    message,
                    retry_after,
                })) => {
                    if attempt >= self.retry.max_attempts {
                        return Err(DownloadError::Fetch(format!(
                            "source `{source}`: {url}: {message} (gave up after {attempt} attempts)"
                        )));
                    }
                    let delay = self.retry.delay(attempt, retry_after);
                    eprintln!(
                        "  {source}: {message}; retry {attempt}/{} in {delay:?}",
                        self.retry.max_attempts - 1
                    );
                    std::thread::sleep(delay);
                    attempt += 1;
                }
            }
        }
    }

    fn try_once(&self, url: &str, part: &Path) -> Result<(u64, String), AttemptError> {
        let mut body = self.fetcher.open(url).map_err(AttemptError::Fetch)?;
        let mut out = File::create(part).map_err(AttemptError::Local)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut total = 0u64;
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(AttemptError::Fetch(FetchError::Transient {
                        message: format!("read failed: {e}"),
                        retry_after: None,
                    }))
                }
            };
            let chunk = &buf[..n];
            out.write_all(chunk).map_err(AttemptError::Local)?;
            hasher.update(chunk);
            total += n as u64;
        }
        out.flush().map_err(AttemptError::Local)?;
        Ok((total, hasher.finalize().to_hex().to_string()))
    }
}

enum AttemptError {
    Fetch(FetchError),
    Local(std::io::Error),
}

/// In-memory fetcher for tests (here and in `build`).
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    #[derive(Debug, Default)]
    pub struct FakeFetcher {
        pub files: RefCell<HashMap<String, Vec<u8>>>,
        pub calls: RefCell<Vec<String>>,
        /// Number of initial calls that fail transiently.
        pub fail_first: Cell<u32>,
    }

    impl FakeFetcher {
        pub fn with(url: &str, bytes: Vec<u8>) -> FakeFetcher {
            let f = FakeFetcher::default();
            f.files.borrow_mut().insert(url.to_string(), bytes);
            f
        }
        pub fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl Fetcher for FakeFetcher {
        fn open(&self, url: &str) -> Result<Box<dyn Read>, FetchError> {
            self.calls.borrow_mut().push(url.to_string());
            if self.fail_first.get() > 0 {
                self.fail_first.set(self.fail_first.get() - 1);
                return Err(FetchError::Transient {
                    message: "HTTP 503".into(),
                    retry_after: None,
                });
            }
            match self.files.borrow().get(url) {
                Some(b) => Ok(Box::new(std::io::Cursor::new(b.clone()))),
                None => Err(FetchError::Permanent("HTTP 404".into())),
            }
        }
    }

    pub fn fast_retry() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    const URL: &str = "https://example.org/a.bin";

    fn pin(bytes: &[u8]) -> LockEntry {
        LockEntry {
            blake3: blake3::hash(bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
            source: "s".into(),
            url: URL.into(),
        }
    }

    fn dl<'a>(f: &'a FakeFetcher, dir: &Path) -> Downloader<'a> {
        Downloader::new(f, dir.to_path_buf(), fast_retry(), Profile::Small)
    }

    #[test]
    fn user_agent_format() {
        let ua = user_agent();
        assert!(ua.starts_with("lpk-bench/"));
        assert!(ua.ends_with(" (+https://github.com/Charon89/litepack)"));
    }

    #[test]
    fn verified_download_then_cache_hit() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, b"hello".to_vec());
        let d = dl(&f, dir.path());
        let p = pin(b"hello");
        let a = d.obtain("s", URL, Expect::Pinned(&p)).expect("obtain");
        assert_eq!(a.bytes, 5);
        assert_eq!(f.call_count(), 1);
        d.obtain("s", URL, Expect::Pinned(&p)).expect("cached");
        assert_eq!(f.call_count(), 1, "second call must come from the cache");
        assert!(!a.path.with_extension("part").exists());
    }

    #[test]
    fn wrong_hash_is_a_mismatch_error_naming_source_and_repin() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, b"hello".to_vec());
        let d = dl(&f, dir.path());
        let err = d
            .obtain("my-src", URL, Expect::Pinned(&pin(b"other")))
            .expect_err("mismatch");
        let msg = err.to_string();
        assert!(matches!(err, DownloadError::Mismatch(_)));
        assert!(
            msg.contains("my-src") && msg.contains("--update-lock"),
            "{msg}"
        );
        assert!(
            !d.cache_path("my-src", URL).exists(),
            "bad bytes must not enter the cache"
        );
    }

    #[test]
    fn corrupt_cache_entry_is_refetched_not_trusted() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, b"hello".to_vec());
        let d = dl(&f, dir.path());
        let p = pin(b"hello");
        let a = d.obtain("s", URL, Expect::Pinned(&p)).expect("obtain");
        std::fs::write(&a.path, b"HELLO").expect("corrupt");
        d.obtain("s", URL, Expect::Pinned(&p)).expect("refetch");
        assert_eq!(f.call_count(), 2);
        assert_eq!(std::fs::read(&a.path).expect("read"), b"hello");
    }

    #[test]
    fn unpinned_always_downloads() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, b"hello".to_vec());
        let d = dl(&f, dir.path());
        d.obtain("s", URL, Expect::Unpinned).expect("one");
        d.obtain("s", URL, Expect::Unpinned).expect("two");
        assert_eq!(f.call_count(), 2);
    }

    #[test]
    fn transient_errors_retry_then_succeed_or_give_up() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, b"hello".to_vec());
        f.fail_first.set(2);
        dl(&f, dir.path())
            .obtain("s", URL, Expect::Unpinned)
            .expect("third try works");
        assert_eq!(f.call_count(), 3);

        let f = FakeFetcher::with(URL, b"hello".to_vec());
        f.fail_first.set(10);
        let err = dl(&f, dir.path())
            .obtain("s", URL, Expect::Unpinned)
            .expect_err("gives up");
        assert!(matches!(err, DownloadError::Fetch(_)));
        assert_eq!(f.call_count(), 3);
    }

    #[test]
    fn permanent_errors_do_not_retry() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::default();
        let err = dl(&f, dir.path())
            .obtain("s", URL, Expect::Unpinned)
            .expect_err("404");
        assert!(matches!(err, DownloadError::Fetch(_)));
        assert_eq!(f.call_count(), 1);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(10),
        };
        assert_eq!(p.delay(1, None), Duration::from_secs(2));
        assert_eq!(p.delay(2, None), Duration::from_secs(4));
        assert_eq!(p.delay(5, None), Duration::from_secs(10));
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(700))),
            Duration::from_secs(10)
        );
    }
}
