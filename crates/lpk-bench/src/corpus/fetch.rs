//! Downloading: the [`Fetcher`] abstraction, the HTTPS implementation, and the verified cache.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::lock::LockEntry;
use super::registry::{HostSpec, Profile};

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
    /// The server says the resource is gone (HTTP 404 or 410).
    Gone(String),
    /// The partial file cannot be continued (416, or a `206` that is not exactly the rest of
    /// the file): the downloader must request again from zero, through its politeness wait.
    Restart,
}

/// Source of bytes for a URL. Tests implement this over memory; production uses [`HttpFetcher`].
pub trait Fetcher: fmt::Debug {
    /// Open a stream of the response body, starting at byte `offset` if the server supports it
    /// (`offset == 0` for a fresh download). [`Opened::start`] says where the stream really
    /// starts, so a server that ignores the range simply yields `start == 0`. `if_range` is the
    /// validator ([`Opened::validator`] of the first response) that must still match for the
    /// server to honour the range; otherwise it sends the whole body again.
    fn open(&self, url: &str, offset: u64, if_range: Option<&str>) -> Result<Opened, FetchError>;
}

/// An open response body.
pub struct Opened {
    pub body: Box<dyn Read>,
    /// Byte offset of the first byte of `body` within the resource.
    pub start: u64,
    /// Strong `ETag`, else `Last-Modified`, of this response (usable as `If-Range`).
    pub validator: Option<String>,
    /// `Retry-After` header of this response, if any (an API may ask to wait even with 200).
    pub retry_after: Option<Duration>,
}

/// In-process HTTPS client (no external curl).
#[derive(Debug)]
pub struct HttpFetcher {
    client: reqwest::blocking::Client,
}

/// User-Agent sent on every request.
pub fn user_agent() -> String {
    format!(
        "lpk-bench/{} (https://github.com/Charon89/litepack; corpus-builder bot)",
        env!("CARGO_PKG_VERSION")
    )
}

impl HttpFetcher {
    pub fn new() -> anyhow::Result<HttpFetcher> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(30))
            // In blocking reqwest this limit applies to waiting for the headers and to each
            // body `read()`, i.e. it is an idle timeout, not a limit on the whole download.
            // A timeout is retried (and resumed) like any transient error.
            .timeout(Duration::from_secs(90))
            // Redirects must not downgrade to plain HTTP: pinned bytes come over TLS only.
            .https_only(true)
            .build()?;
        Ok(HttpFetcher { client })
    }
}

impl Fetcher for HttpFetcher {
    fn open(&self, url: &str, offset: u64, if_range: Option<&str>) -> Result<Opened, FetchError> {
        let mut req = self.client.get(url);
        if offset > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            if let Some(v) = if_range {
                req = req.header(reqwest::header::IF_RANGE, v);
            }
        }
        let resp = req.send().map_err(|e| {
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
        let validator = response_validator(
            resp.headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok()),
            resp.headers()
                .get(reqwest::header::LAST_MODIFIED)
                .and_then(|v| v.to_str().ok()),
        );
        let header_retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| parse_retry_after(v, std::time::SystemTime::now()));
        if status.as_u16() == 206 {
            let ok = offset > 0
                && resp
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| is_exact_tail(v, offset));
            if ok {
                return Ok(Opened {
                    body: Box::new(resp),
                    start: offset,
                    validator,
                    retry_after: None,
                });
            }
            drop(resp);
            if offset > 0 {
                // A range that is not exactly the rest of the file: start over.
                return Err(FetchError::Restart);
            }
            return Err(FetchError::Permanent(
                "server sent a partial response to a plain request".to_string(),
            ));
        }
        if status.as_u16() == 416 && offset > 0 {
            // The partial file is not usable for this resource; start over.
            return Err(FetchError::Restart);
        }
        if status.is_success() {
            return Ok(Opened {
                body: Box::new(resp),
                start: 0,
                validator,
                retry_after: header_retry_after,
            });
        }
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(FetchError::Transient {
                message: format!("HTTP {status}"),
                retry_after: header_retry_after,
            });
        }
        if matches!(status.as_u16(), 404 | 410) {
            return Err(FetchError::Gone(format!("HTTP {status}")));
        }
        Err(FetchError::Permanent(format!("HTTP {status}")))
    }
}

/// `Retry-After`: delta-seconds or an HTTP date (relative to `now`; a past date is zero).
pub fn parse_retry_after(v: &str, now: std::time::SystemTime) -> Option<Duration> {
    let v = v.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(v).ok()?;
    Some(when.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Parse `bytes <start>-<end>/<total>`.
pub fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?, total))
}

/// True when a `Content-Range` is exactly the rest of the file from `offset`: it starts at
/// `offset`, and ends at the last byte of a known total.
pub fn is_exact_tail(v: &str, offset: u64) -> bool {
    matches!(
        parse_content_range(v),
        Some((start, end, Some(total))) if start == offset && end.checked_add(1) == Some(total)
    )
}

/// Validator for `If-Range`: a strong `ETag` if present (weak ones cannot be used), else
/// `Last-Modified`.
pub fn response_validator(etag: Option<&str>, last_modified: Option<&str>) -> Option<String> {
    etag.filter(|e| !e.trim_start().starts_with("W/"))
        .or(last_modified)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Politeness limits for one host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostPolicy {
    /// Minimum time between the end of one request and the start of the next.
    pub min_interval: Duration,
    /// Throughput cap in bytes per second.
    pub max_bytes_per_s: Option<u64>,
    /// Attempts per request, replacing [`RetryPolicy::max_attempts`] for this host.
    pub max_attempts: Option<u32>,
    /// Cap on one back-off wait, replacing [`RetryPolicy::max_delay`] for this host.
    pub max_delay: Option<Duration>,
}

/// Per-host request pacing, applied by the [`Downloader`] to every request it makes.
#[derive(Debug, Default)]
pub struct Politeness {
    hosts: BTreeMap<String, HostPolicy>,
    last: RefCell<BTreeMap<String, Instant>>,
}

/// Lower-case host of an `https://` URL.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

impl Politeness {
    pub fn new(hosts: BTreeMap<String, HostPolicy>) -> Politeness {
        Politeness {
            hosts,
            last: RefCell::new(BTreeMap::new()),
        }
    }

    /// Build from the registry's `[[host]]` tables.
    pub fn from_specs(specs: &[HostSpec]) -> Politeness {
        Politeness::new(
            specs
                .iter()
                .map(|h| {
                    (
                        h.name.clone(),
                        HostPolicy {
                            min_interval: Duration::from_millis(h.min_interval_ms),
                            max_bytes_per_s: h.max_mbit_per_s.map(|m| m.saturating_mul(125_000)),
                            max_attempts: h.max_attempts,
                            max_delay: h.max_delay_ms.map(Duration::from_millis),
                        },
                    )
                })
                .collect(),
        )
    }

    fn policy(&self, url: &str) -> Option<(String, HostPolicy)> {
        let host = host_of(url)?;
        let p = *self.hosts.get(&host)?;
        Some((host, p))
    }

    /// Block until `min_interval` has passed since the previous request to this host ended.
    pub fn wait(&self, url: &str) {
        let Some((host, p)) = self.policy(url) else {
            return;
        };
        let last = self.last.borrow().get(&host).copied();
        if let Some(last) = last {
            let since = last.elapsed();
            if since < p.min_interval {
                std::thread::sleep(p.min_interval - since);
            }
        }
        self.last.borrow_mut().insert(host, Instant::now());
    }

    /// Record the end of a request (success or failure).
    pub fn finish(&self, url: &str) {
        if let Some((host, _)) = self.policy(url) {
            self.last.borrow_mut().insert(host, Instant::now());
        }
    }

    /// `base` with the host's own attempts and back-off cap, when it sets them.
    fn retry_for(&self, url: &str, base: RetryPolicy) -> RetryPolicy {
        match self.policy(url) {
            Some((_, p)) => RetryPolicy {
                max_attempts: p.max_attempts.unwrap_or(base.max_attempts),
                max_delay: p.max_delay.unwrap_or(base.max_delay),
                ..base
            },
            None => base,
        }
    }

    fn cap(&self, url: &str) -> Option<u64> {
        self.policy(url).and_then(|(_, p)| p.max_bytes_per_s)
    }
}

/// Retry behaviour: exponential backoff from `base_delay`, capped at `max_delay`.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// Longest `Retry-After` the tool waits for; a larger request fails the download with a
    /// clear error (never an early retry).
    pub max_retry_after: Duration,
    /// Smallest wait after an API answered `maxlag` (MediaWiki asks for at least 5 s).
    pub maxlag_min: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
            max_retry_after: Duration::from_secs(900),
            maxlag_min: Duration::from_secs(5),
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
        retry_after.map_or(backoff, |r| r.max(backoff))
    }

    /// Wait after a `maxlag` answer: never below `maxlag_min`, nor below the server's request.
    pub fn maxlag_delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        self.delay(attempt, retry_after).max(self.maxlag_min)
    }

    /// Err with a message when the server asks for a longer wait than this tool is willing to
    /// give.
    pub fn check_retry_after(&self, retry_after: Option<Duration>) -> Result<(), String> {
        match retry_after {
            Some(r) if r > self.max_retry_after => Err(format!(
                "the server asked to wait {} s (Retry-After), more than the {} s this tool \
                 waits; try again later",
                r.as_secs(),
                self.max_retry_after.as_secs()
            )),
            _ => Ok(()),
        }
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
    /// The server answered 404 or 410: the file is gone upstream.
    Gone(String),
    Mismatch(String),
    Io(String),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::Fetch(m)
            | DownloadError::Gone(m)
            | DownloadError::Mismatch(m)
            | DownloadError::Io(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for DownloadError {}

/// How to re-pin: the command to run after reviewing upstream changes.
pub fn repin_hint(profile: Profile) -> String {
    format!(
        "Re-pin with `lpk-bench corpus build --profile {p} --update-lock` and commit \
         bench/corpus.lock (`--only <class>` pins just that class but writes only \
         manifest.partial.json).",
        p = profile.name()
    )
}

/// Temporary download file for a cache path: the full file name plus `.part`
/// (`with_extension` would clobber a `.` inside the name).
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    path.with_file_name(name)
}

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
    politeness: Politeness,
    /// Back-off waits requested instead of slept (tests only).
    #[cfg(test)]
    paused: RefCell<Vec<Duration>>,
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
            politeness: Politeness::default(),
            #[cfg(test)]
            paused: RefCell::new(Vec::new()),
        }
    }

    /// Wait out a back-off delay (recorded instead of slept under test).
    fn pause(&self, delay: Duration) {
        #[cfg(test)]
        self.paused.borrow_mut().push(delay);
        #[cfg(not(test))]
        std::thread::sleep(delay);
    }

    /// The retry rules for requests to `url`'s host.
    fn retry_for(&self, url: &str) -> RetryPolicy {
        self.politeness.retry_for(url, self.retry)
    }

    /// Apply per-host pacing to every request this downloader makes.
    pub fn with_politeness(mut self, politeness: Politeness) -> Self {
        self.politeness = politeness;
        self
    }

    /// The download cache directory.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Sleep for the configured minimum interval of `url`'s host (zero when none is set). A
    /// resolver uses it to space out requests that go to a different host of the same operator.
    pub fn settle(&self, url: &str) {
        if let Some((_, p)) = self.politeness.policy(url) {
            std::thread::sleep(p.min_interval);
        }
    }

    /// Sleep after an API said `maxlag` (attempt counted from 1): at least `maxlag_min` and
    /// the server's `Retry-After`.
    pub fn maxlag_wait(&self, attempt: u32, retry_after: Option<Duration>) {
        std::thread::sleep(self.retry.maxlag_delay(attempt, retry_after));
    }

    /// GET a small resource (an API response) into memory, with the same per-host pacing and
    /// retry rules as downloads. At most `limit` bytes are accepted. Never cached.
    pub fn get_bytes(&self, source: &str, url: &str, limit: u64) -> Result<Vec<u8>, DownloadError> {
        self.get_response(source, url, limit).map(|r| r.0)
    }

    /// Like [`Downloader::get_bytes`], also returning the `Retry-After` of the final response.
    pub fn get_response(
        &self,
        source: &str,
        url: &str,
        limit: u64,
    ) -> Result<(Vec<u8>, Option<Duration>), DownloadError> {
        let retry = self.retry_for(url);
        let mut attempt = 1;
        loop {
            self.politeness.wait(url);
            let outcome = self.fetcher.open(url, 0, None).and_then(|opened| {
                let mut buf = Vec::new();
                let retry_after = opened.retry_after;
                opened
                    .body
                    .take(limit.saturating_add(1))
                    .read_to_end(&mut buf)
                    .map_err(|e| FetchError::Transient {
                        message: format!("read failed: {e}"),
                        retry_after: None,
                    })?;
                if buf.len() as u64 > limit {
                    return Err(FetchError::Permanent(format!(
                        "response exceeds {limit} bytes"
                    )));
                }
                Ok((buf, retry_after))
            });
            self.politeness.finish(url);
            match outcome {
                Ok(b) => {
                    if let Err(m) = retry.check_retry_after(b.1) {
                        return Err(DownloadError::Fetch(format!(
                            "source `{source}`: {url}: {m}"
                        )));
                    }
                    return Ok(b);
                }
                Err(FetchError::Permanent(m)) => {
                    return Err(DownloadError::Fetch(format!(
                        "source `{source}`: {url}: {m}"
                    )))
                }
                Err(FetchError::Gone(m)) => {
                    return Err(DownloadError::Gone(format!(
                        "source `{source}`: {url}: {m}"
                    )))
                }
                Err(FetchError::Restart) => {
                    return Err(DownloadError::Fetch(format!(
                        "source `{source}`: {url}: unexpected range response"
                    )))
                }
                Err(FetchError::Transient {
                    message,
                    retry_after,
                }) => {
                    if attempt >= retry.max_attempts {
                        return Err(DownloadError::Fetch(format!(
                            "source `{source}`: {url}: {message} (gave up after {attempt} attempts)"
                        )));
                    }
                    if let Err(m) = retry.check_retry_after(retry_after) {
                        return Err(DownloadError::Fetch(format!(
                            "source `{source}`: {url}: {message}: {m}"
                        )));
                    }
                    let delay = retry.delay(attempt, retry_after);
                    eprintln!(
                        "  {source}: {message}; retry {attempt}/{} in {delay:?}",
                        retry.max_attempts - 1
                    );
                    self.pause(delay);
                    attempt += 1;
                }
            }
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

        let pinned = match expect {
            Expect::Pinned(pin) => match (pin.bytes, pin.blake3.as_deref()) {
                (Some(b), Some(h)) => Some((b, h)),
                _ => {
                    return Err(DownloadError::Mismatch(format!(
                        "source `{source}`: lock entry for {url} has no bytes/blake3 to verify \
                         a download against. {}",
                        repin_hint(self.profile)
                    )))
                }
            },
            Expect::Unpinned => None,
        };

        if let Some((pin_bytes, pin_hash)) = pinned {
            if path.is_file() {
                let (bytes, blake3) = hash_file(&path).map_err(io)?;
                if bytes == pin_bytes && blake3 == pin_hash {
                    return Ok(Artifact {
                        path,
                        bytes,
                        blake3,
                    });
                }
                eprintln!("  cache for `{source}` does not match the lock; fetching again");
            }
        }

        let part = part_path(&path);
        let (bytes, blake3) = self.download(source, url, &part, pinned.is_some())?;
        if let Some((pin_bytes, pin_hash)) = pinned {
            if bytes != pin_bytes || blake3 != pin_hash {
                let _ = std::fs::remove_file(&part);
                return Err(DownloadError::Mismatch(format!(
                    "source `{source}`: {url} does not match bench/corpus.lock \
                     (locked {pin_bytes} bytes blake3 {pin_hash}, got {bytes} bytes blake3 \
                     {blake3}). If upstream legitimately changed, review it first. {}",
                    repin_hint(self.profile)
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
        pinned: bool,
    ) -> Result<(u64, String), DownloadError> {
        // A leftover from an earlier run is never resumed; within this call a failed attempt
        // keeps its partial file and the next attempt continues from it.
        let _ = std::fs::remove_file(part);
        let retry = self.retry_for(url);
        let mut attempt = 1;
        let mut validator: Option<String> = None;
        let mut restarts = 0u32;
        loop {
            self.politeness.wait(url);
            let outcome = self.try_once(url, part, &mut validator, pinned);
            self.politeness.finish(url);
            match outcome {
                Ok(r) => return Ok(r),
                Err(AttemptError::Local(e)) => {
                    return Err(DownloadError::Io(format!("source `{source}`: {e}")))
                }
                Err(AttemptError::Fetch(FetchError::Restart)) => {
                    restarts += 1;
                    if restarts > 3 {
                        return Err(DownloadError::Fetch(format!(
                            "source `{source}`: {url}: the server keeps refusing to resume"
                        )));
                    }
                    // Discard the partial file; the next loop pass waits like any request.
                    let _ = std::fs::remove_file(part);
                    validator = None;
                }
                Err(AttemptError::Fetch(FetchError::Gone(m))) => {
                    return Err(DownloadError::Gone(format!(
                        "source `{source}`: {url}: {m}"
                    )))
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
                    if attempt >= retry.max_attempts {
                        return Err(DownloadError::Fetch(format!(
                            "source `{source}`: {url}: {message} (gave up after {attempt} attempts)"
                        )));
                    }
                    if let Err(m) = retry.check_retry_after(retry_after) {
                        return Err(DownloadError::Fetch(format!(
                            "source `{source}`: {url}: {message}: {m}"
                        )));
                    }
                    let delay = retry.delay(attempt, retry_after);
                    eprintln!(
                        "  {source}: {message}; retry {attempt}/{} in {delay:?}",
                        retry.max_attempts - 1
                    );
                    self.pause(delay);
                    attempt += 1;
                }
            }
        }
    }

    /// One attempt. `validator` is the `ETag`/`Last-Modified` of the first response; a partial
    /// file is resumed (with `If-Range`) only when there is one, or when the download is pinned
    /// (the final hash is then checked anyway); otherwise the attempt restarts from zero.
    fn try_once(
        &self,
        url: &str,
        part: &Path,
        validator: &mut Option<String>,
        pinned: bool,
    ) -> Result<(u64, String), AttemptError> {
        let have = std::fs::metadata(part).map_or(0, |m| m.len());
        let offset = if have > 0 && validator.is_none() && !pinned {
            0
        } else {
            have
        };
        let if_range = if offset > 0 {
            validator.as_deref()
        } else {
            None
        };
        let opened = self
            .fetcher
            .open(url, offset, if_range)
            .map_err(AttemptError::Fetch)?;
        let Opened {
            mut body,
            start,
            validator: seen,
            ..
        } = opened;
        if start == 0 {
            *validator = seen;
        } else if start != offset {
            return Err(AttemptError::Fetch(FetchError::Transient {
                message: format!("server resumed at {start}, expected {offset}"),
                retry_after: None,
            }));
        }
        let began = Instant::now();
        let cap = self.politeness.cap(url);
        let mut read_now = 0u64;
        let mut hasher = blake3::Hasher::new();
        let mut total = 0u64;
        let mut out = if offset > 0 && start == offset {
            // Resume: re-hash what is already on disk, then append.
            let mut existing = File::open(part).map_err(AttemptError::Local)?;
            total = std::io::copy(&mut existing, &mut hasher).map_err(AttemptError::Local)?;
            if total != have {
                return Err(AttemptError::Local(std::io::Error::other(
                    "partial file changed while resuming",
                )));
            }
            File::options()
                .append(true)
                .open(part)
                .map_err(AttemptError::Local)?
        } else {
            // Fresh start, or the server ignored the range: restart from zero.
            File::create(part).map_err(AttemptError::Local)?
        };
        let mut buf = vec![0u8; 1 << 16];
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
            read_now += n as u64;
            if let Some(cap) = cap {
                let due = Duration::from_secs_f64(read_now as f64 / cap as f64);
                if let Some(wait) = due.checked_sub(began.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
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
        /// If set, the next successful `open` yields only this many bytes and then an I/O error.
        pub cut_next_after: Cell<Option<usize>>,
        /// Pretend the server ignores `Range` and always sends the whole body.
        pub ignore_range: Cell<bool>,
        /// `offset` argument of every `open` call.
        pub offsets: RefCell<Vec<u64>>,
        /// Validator (`ETag`) the pretend server reports.
        pub validator: RefCell<Option<String>>,
        /// `if_range` argument of every `open` call.
        pub if_ranges: RefCell<Vec<Option<String>>>,
        /// Answer every ranged request (`offset > 0`) with a restart signal.
        pub restart_on_resume: Cell<bool>,
        /// Change the validator when a cut-off response is produced (the file "changed").
        pub rotate_on_cut: Cell<bool>,
        /// `Retry-After` the pretend server attaches to transient failures and to answers.
        pub retry_after: Cell<Option<Duration>>,
    }

    /// Yields `data`, then fails (a connection dropped mid-body).
    struct DropAfter(std::io::Cursor<Vec<u8>>);

    impl Read for DropAfter {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.read(buf)? {
                0 => Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "reset",
                )),
                n => Ok(n),
            }
        }
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
        fn open(
            &self,
            url: &str,
            offset: u64,
            if_range: Option<&str>,
        ) -> Result<Opened, FetchError> {
            self.calls.borrow_mut().push(url.to_string());
            self.offsets.borrow_mut().push(offset);
            self.if_ranges
                .borrow_mut()
                .push(if_range.map(str::to_string));
            if offset > 0 && self.restart_on_resume.get() {
                return Err(FetchError::Restart);
            }
            if self.fail_first.get() > 0 {
                self.fail_first.set(self.fail_first.get() - 1);
                return Err(FetchError::Transient {
                    message: "HTTP 503".into(),
                    retry_after: self.retry_after.get(),
                });
            }
            let files = self.files.borrow();
            let Some(bytes) = files.get(url) else {
                return Err(FetchError::Gone("HTTP 404".into()));
            };
            let current = self.validator.borrow().clone();
            let changed = if_range.is_some() && current.as_deref() != if_range;
            let start = if self.ignore_range.get() || changed {
                0
            } else {
                offset
            };
            let mut data = bytes[start as usize..].to_vec();
            if let Some(n) = self.cut_next_after.take() {
                data.truncate(n);
                if self.rotate_on_cut.get() {
                    *self.validator.borrow_mut() = Some("\"v2\"".into());
                }
                return Ok(Opened {
                    body: Box::new(DropAfter(std::io::Cursor::new(data))),
                    start,
                    validator: current,
                    retry_after: None,
                });
            }
            Ok(Opened {
                body: Box::new(std::io::Cursor::new(data)),
                start,
                validator: current,
                retry_after: self.retry_after.get(),
            })
        }
    }

    pub fn fast_retry() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_retry_after: Duration::from_secs(900),
            maxlag_min: Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    const URL: &str = "https://example.org/a.bin";

    fn pin(bytes: &[u8]) -> LockEntry {
        LockEntry::artifact(
            "s",
            URL,
            bytes.len() as u64,
            blake3::hash(bytes).to_hex().to_string(),
        )
    }

    fn dl<'a>(f: &'a FakeFetcher, dir: &Path) -> Downloader<'a> {
        Downloader::new(f, dir.to_path_buf(), fast_retry(), Profile::Small)
    }

    #[test]
    fn user_agent_format() {
        let ua = user_agent();
        assert!(ua.starts_with("lpk-bench/"));
        assert!(ua.ends_with(" (https://github.com/Charon89/litepack; corpus-builder bot)"));
        assert!(ua.contains("bot"));
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
        assert!(matches!(err, DownloadError::Gone(_)));
        assert_eq!(f.call_count(), 1);
    }

    #[test]
    fn mid_body_failure_resumes_from_the_partial_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let f = FakeFetcher::with(URL, data.clone());
        f.cut_next_after.set(Some(1234));
        let a = dl(&f, dir.path())
            .obtain("s", URL, Expect::Pinned(&pin(&data)))
            .expect("obtain");
        assert_eq!(
            *f.offsets.borrow(),
            [0, 1234],
            "second attempt must ask for a range"
        );
        assert_eq!(std::fs::read(&a.path).expect("read"), data);
        assert_eq!(a.blake3, blake3::hash(&data).to_hex().to_string());
    }

    #[test]
    fn server_that_ignores_range_restarts_cleanly() {
        let dir = tempfile::tempdir().expect("tmp");
        let data: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
        let f = FakeFetcher::with(URL, data.clone());
        f.ignore_range.set(true);
        f.cut_next_after.set(Some(700));
        let a = dl(&f, dir.path())
            .obtain("s", URL, Expect::Pinned(&pin(&data)))
            .expect("obtain");
        assert_eq!(*f.offsets.borrow(), [0, 700]);
        assert_eq!(
            std::fs::read(&a.path).expect("read"),
            data,
            "no duplicated prefix"
        );
    }

    #[test]
    fn part_files_are_per_cache_name_even_with_dots_in_ids() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::default();
        let d = dl(&f, dir.path());
        let (a, b) = (d.cache_path("py.1", URL), d.cache_path("py.2", URL));
        let (pa, pb) = (part_path(&a), part_path(&b));
        assert_ne!(pa, pb);
        assert_eq!(
            pa.file_name().and_then(|n| n.to_str()).map(str::to_string),
            Some(format!(
                "{}.part",
                a.file_name().and_then(|n| n.to_str()).unwrap_or("")
            ))
        );
    }

    #[test]
    fn lock_entry_without_a_hash_is_a_hard_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, b"x".to_vec());
        let mut p = pin(b"x");
        p.blake3 = None;
        let err = dl(&f, dir.path())
            .obtain("s", URL, Expect::Pinned(&p))
            .expect_err("no hash");
        assert!(matches!(err, DownloadError::Mismatch(_)));
        assert_eq!(f.call_count(), 0);
    }

    #[test]
    fn http_client_builds_with_https_only_and_idle_timeout() {
        HttpFetcher::new().expect("client");
    }

    #[test]
    fn backoff_doubles_and_never_retries_before_retry_after() {
        let p = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(10),
            max_retry_after: Duration::from_secs(900),
            maxlag_min: Duration::from_secs(5),
        };
        assert_eq!(p.delay(1, None), Duration::from_secs(2));
        assert_eq!(p.delay(2, None), Duration::from_secs(4));
        assert_eq!(p.delay(5, None), Duration::from_secs(10));
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(300))),
            Duration::from_secs(300),
            "the full Retry-After is waited, not cut to the back-off cap"
        );
        assert!(p.check_retry_after(Some(Duration::from_secs(900))).is_ok());
        let e = p
            .check_retry_after(Some(Duration::from_secs(901)))
            .expect_err("too long");
        assert!(e.contains("Retry-After"), "{e}");
        // maxlag: at least five seconds, more when the server says so.
        assert_eq!(p.maxlag_delay(1, None), Duration::from_secs(5));
        assert_eq!(
            p.maxlag_delay(1, Some(Duration::from_secs(30))),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn retry_after_parses_seconds_and_http_dates() {
        use std::time::{Duration as D, SystemTime, UNIX_EPOCH};
        let now = UNIX_EPOCH + D::from_secs(784_111_700);
        assert_eq!(parse_retry_after(" 120 ", now), Some(D::from_secs(120)));
        // 784_111_777 = Sun, 06 Nov 1994 08:49:37 GMT
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now),
            Some(D::from_secs(784_111_777 - 784_111_700))
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", SystemTime::now()),
            Some(D::ZERO),
            "a date in the past means no wait"
        );
        assert_eq!(parse_retry_after("soon", now), None);
    }

    #[test]
    fn too_long_retry_after_fails_clearly_instead_of_retrying_early() {
        let f = FakeFetcher::with("https://example.org/a", b"x".to_vec());
        f.fail_first.set(1);
        f.retry_after.set(Some(Duration::from_secs(5000)));
        let dir = tempfile::tempdir().expect("tmp");
        let d = dl(&f, dir.path());
        let err = d
            .get_bytes("s", "https://example.org/a", 100)
            .expect_err("refused");
        assert!(err.to_string().contains("Retry-After"), "{err}");
        assert_eq!(f.call_count(), 1, "no early retry");
        f.fail_first.set(1);
        let err = d
            .obtain("s", "https://example.org/a", Expect::Unpinned)
            .expect_err("refused");
        assert!(err.to_string().contains("Retry-After"), "{err}");
        assert_eq!(f.call_count(), 2);
    }

    #[test]
    fn content_range_must_be_exactly_the_rest_of_the_file() {
        assert_eq!(
            parse_content_range("bytes 100-199/200"),
            Some((100, 199, Some(200)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("items 1-2/3"), None);
        assert!(is_exact_tail("bytes 100-199/200", 100));
        assert!(!is_exact_tail("bytes 100-150/200", 100), "ends early");
        assert!(!is_exact_tail("bytes 90-199/200", 100), "wrong start");
        assert!(!is_exact_tail("bytes 100-199/*", 100), "unknown total");
        assert!(!is_exact_tail("garbage", 100));
        assert_eq!(
            response_validator(Some("W/\"weak\""), Some("Mon, 01 Jan 2024 00:00:00 GMT"))
                .as_deref(),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );
        assert_eq!(
            response_validator(Some("\"abc\""), None).as_deref(),
            Some("\"abc\"")
        );
        assert_eq!(response_validator(None, None), None);
    }

    #[test]
    fn unpinned_download_resumes_only_with_a_validator() {
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        // No validator: restart from zero instead of resuming an unverifiable partial file.
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, data.clone());
        f.cut_next_after.set(Some(1234));
        let a = dl(&f, dir.path())
            .obtain("s", URL, Expect::Unpinned)
            .expect("obtain");
        assert_eq!(*f.offsets.borrow(), [0, 0]);
        assert_eq!(std::fs::read(&a.path).expect("read"), data);
        // With a validator: resume, sending it as If-Range.
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, data.clone());
        *f.validator.borrow_mut() = Some("\"v1\"".into());
        f.cut_next_after.set(Some(1234));
        let a = dl(&f, dir.path())
            .obtain("s", URL, Expect::Unpinned)
            .expect("obtain");
        assert_eq!(*f.offsets.borrow(), [0, 1234]);
        assert_eq!(f.if_ranges.borrow()[1].as_deref(), Some("\"v1\""));
        assert_eq!(std::fs::read(&a.path).expect("read"), data);
        // The resource changes between attempts: the server ignores the stale If-Range and
        // sends everything, which replaces the partial file instead of being appended to it.
        let dir = tempfile::tempdir().expect("tmp");
        let f = FakeFetcher::with(URL, data.clone());
        *f.validator.borrow_mut() = Some("\"v1\"".into());
        f.cut_next_after.set(Some(700));
        f.rotate_on_cut.set(true);
        let a = dl(&f, dir.path())
            .obtain("s", URL, Expect::Unpinned)
            .expect("obtain");
        assert_eq!(
            *f.offsets.borrow(),
            [0, 700],
            "resume attempted, then served in full"
        );
        assert_eq!(
            std::fs::read(&a.path).expect("read"),
            data,
            "no spliced bytes"
        );
    }

    #[test]
    fn per_host_interval_and_throughput_cap_are_enforced() {
        let hosts = BTreeMap::from([(
            "slow.example".to_string(),
            HostPolicy {
                min_interval: Duration::from_millis(120),
                max_bytes_per_s: Some(100_000),
                ..HostPolicy::default()
            },
        )]);
        let dir = tempfile::tempdir().expect("tmp");
        let (u1, u2) = ("https://slow.example/a", "https://slow.example/b");
        let f = FakeFetcher::with(u1, vec![1u8; 20_000]);
        f.files.borrow_mut().insert(u2.into(), vec![2u8; 20_000]);
        f.files
            .borrow_mut()
            .insert("https://other.example/c".into(), vec![3u8; 10]);
        let d = dl(&f, dir.path()).with_politeness(Politeness::new(hosts));
        let t = Instant::now();
        d.obtain("s", u1, Expect::Unpinned).expect("a");
        assert!(
            t.elapsed() >= Duration::from_millis(200),
            "20 kB at 100 kB/s"
        );
        let t = Instant::now();
        d.obtain("s", "https://other.example/c", Expect::Unpinned)
            .expect("c");
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "unlisted hosts are not paced"
        );
        let t = Instant::now();
        d.obtain("s2", u2, Expect::Unpinned).expect("b");
        assert!(t.elapsed() >= Duration::from_millis(200));
        assert_eq!(
            host_of("https://User@Slow.Example:8443/x?y"),
            Some("slow.example".into())
        );
        assert_eq!(host_of("http://nope/"), None);
    }

    #[test]
    fn restart_after_a_bad_range_goes_through_the_politeness_wait() {
        let data: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
        let url = "https://polite.example/a.bin";
        let f = FakeFetcher::with(url, data.clone());
        f.restart_on_resume.set(true);
        f.cut_next_after.set(Some(500));
        let hosts = BTreeMap::from([(
            "polite.example".to_string(),
            HostPolicy {
                min_interval: Duration::from_millis(100),
                ..HostPolicy::default()
            },
        )]);
        let dir = tempfile::tempdir().expect("tmp");
        let d = dl(&f, dir.path()).with_politeness(Politeness::new(hosts));
        let pin = LockEntry::artifact(
            "s",
            url,
            data.len() as u64,
            blake3::hash(&data).to_hex().to_string(),
        );
        let t = Instant::now();
        let a = d.obtain("s", url, Expect::Pinned(&pin)).expect("obtain");
        assert_eq!(
            *f.offsets.borrow(),
            [0, 500, 0],
            "resume refused, then from zero"
        );
        assert!(
            t.elapsed() >= Duration::from_millis(200),
            "three requests need two 100 ms gaps, the restart included"
        );
        assert_eq!(std::fs::read(&a.path).expect("read"), data);
    }

    fn patient_host(host: &str) -> Politeness {
        Politeness::new(BTreeMap::from([(
            host.to_string(),
            HostPolicy {
                max_attempts: Some(6),
                max_delay: Some(Duration::from_secs(30)),
                ..HostPolicy::default()
            },
        )]))
    }

    fn secs(v: &[u64]) -> Vec<Duration> {
        v.iter().map(|s| Duration::from_secs(*s)).collect()
    }

    fn slow_retry() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
            max_retry_after: Duration::from_secs(900),
            maxlag_min: Duration::ZERO,
        }
    }

    #[test]
    fn host_attempts_and_backoff_cap_apply_to_that_host_only() {
        let dir = tempfile::tempdir().expect("tmp");
        let (patient, other) = ("https://patient.example/a", "https://other.example/a");
        let f = FakeFetcher::with(patient, b"x".to_vec());
        f.files.borrow_mut().insert(other.into(), b"x".to_vec());
        let d = Downloader::new(&f, dir.path().to_path_buf(), slow_retry(), Profile::Small)
            .with_politeness(patient_host("patient.example"));

        // Six attempts, waits double from 2 s and are capped at 30 s.
        f.fail_first.set(5);
        d.obtain("s", patient, Expect::Unpinned)
            .expect("sixth works");
        assert_eq!(f.call_count(), 6);
        assert_eq!(*d.paused.borrow(), secs(&[2, 4, 8, 16, 30]));

        // Gives up after exactly six attempts.
        d.paused.borrow_mut().clear();
        f.calls.borrow_mut().clear();
        f.fail_first.set(100);
        let err = d
            .obtain("s", patient, Expect::Unpinned)
            .expect_err("gives up");
        assert!(
            err.to_string().contains("gave up after 6 attempts"),
            "{err}"
        );
        assert_eq!(f.call_count(), 6);

        // Another host keeps the defaults: three attempts, waits 2 and 4 s.
        d.paused.borrow_mut().clear();
        f.calls.borrow_mut().clear();
        f.fail_first.set(100);
        let err = d
            .obtain("s", other, Expect::Unpinned)
            .expect_err("gives up");
        assert!(
            err.to_string().contains("gave up after 3 attempts"),
            "{err}"
        );
        assert_eq!(f.call_count(), 3);
        assert_eq!(*d.paused.borrow(), secs(&[2, 4]));
    }

    #[test]
    fn api_requests_use_the_host_policy_and_retry_after_is_still_honoured_and_refused() {
        let dir = tempfile::tempdir().expect("tmp");
        let url = "https://patient.example/api";
        let f = FakeFetcher::with(url, b"{}".to_vec());
        let d = Downloader::new(&f, dir.path().to_path_buf(), slow_retry(), Profile::Small)
            .with_politeness(patient_host("patient.example"));
        f.fail_first.set(4);
        f.retry_after.set(Some(Duration::from_secs(45)));
        d.get_bytes("s", url, 100).expect("fifth works");
        assert_eq!(f.call_count(), 5);
        // The wait is never shorter than Retry-After, even above the back-off cap.
        assert_eq!(*d.paused.borrow(), secs(&[45, 45, 45, 45]));

        // Above the (unchanged) Retry-After limit the build fails without retrying.
        f.calls.borrow_mut().clear();
        f.fail_first.set(4);
        f.retry_after.set(Some(Duration::from_secs(901)));
        let err = d.get_bytes("s", url, 100).expect_err("refused");
        assert!(err.to_string().contains("Retry-After"), "{err}");
        assert_eq!(f.call_count(), 1);
    }

    #[test]
    fn default_policy_is_unchanged_without_host_settings() {
        let p = Politeness::new(BTreeMap::from([(
            "plain.example".to_string(),
            HostPolicy::default(),
        )]));
        let base = RetryPolicy::default();
        for url in ["https://plain.example/x", "https://unknown.example/x"] {
            let r = p.retry_for(url, base);
            assert_eq!(r.max_attempts, 5);
            assert_eq!(r.max_delay, Duration::from_secs(60));
        }
    }
}
