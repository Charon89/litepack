//! `kind = "commons-photos"`: camera JPEGs chosen from a Wikimedia Commons category.
//!
//! A list kind: the resolver runs only under `--update-lock` (the lock is the listing, see
//! [`super::build::build_listed`]). It pages through the MediaWiki API one request at a time and
//! keeps files in the order the API returns them until the profile has enough.
//!
//! A file is accepted when all of these hold:
//! * MIME `image/jpeg` and an `https://upload.wikimedia.org/` URL;
//! * `LicenseShortName` maps onto a fixed table of SPDX identifiers: `CC0-1.0`, `CC-BY-x.y` and
//!   `CC-BY-SA-x.y` (with the few national variants SPDX knows); NonCommercial, NoDerivs,
//!   combined licences and anything unknown are rejected. CC BY and CC BY-SA need a non-empty
//!   artist;
//! * Exif `Make` and `Model` are present (a camera original, not a scan or a render);
//! * its byte size lies in the registry's window.
//!
//! Output names are an ASCII slug of the title plus a short hash of the exact title, so titles
//! that differ only in case or in non-ASCII characters never collide.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use super::build::{Ctx, ListedFile};
use super::registry::{CommonsSpec, Source};

/// Largest API response accepted.
const API_LIMIT: u64 = 32 << 20;
/// Pages the API may answer `maxlag` before the resolver gives up.
const MAXLAG_ATTEMPTS: u32 = 6;
/// Upper bound on pages per listing (the whole featured category is a few hundred).
const MAX_PAGES: u32 = 3000;

/// Percent-encode everything except RFC 3986 unreserved characters.
pub(super) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// URL of one API page; `cont` holds the `continue` parameters of the previous answer.
pub(super) fn api_url(category: &str, cont: &BTreeMap<String, String>) -> String {
    let mut url = format!(
        "https://commons.wikimedia.org/w/api.php?action=query&generator=categorymembers\
         &gcmtitle={}&gcmtype=file&gcmlimit=50&prop=imageinfo\
         &iiprop=url|size|mime|sha1|timestamp|extmetadata|commonmetadata\
         &iiextmetadatafilter=LicenseShortName|Artist&format=json&formatversion=2&maxlag=5",
        percent_encode(category)
    );
    for (k, v) in cont {
        url.push('&');
        url.push_str(&percent_encode(k));
        url.push('=');
        url.push_str(&percent_encode(v));
    }
    url
}

/// One file of an API answer, before the acceptance rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    pub title: String,
    pub url: String,
    pub size: u64,
    pub mime: String,
    pub licence_short: String,
    pub artist_html: String,
    pub make: String,
    pub model: String,
    pub sha1: String,
    pub timestamp: String,
}

/// One parsed API answer.
#[derive(Debug, Default)]
pub(super) struct Page {
    pub candidates: Vec<Candidate>,
    /// `continue` parameters when there is another page.
    pub cont: Option<BTreeMap<String, String>>,
    /// The server asked us to come back later (`maxlag`).
    pub maxlag: bool,
}

fn text(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_default()
}

fn exif(info: &Value, name: &str) -> String {
    info["commonmetadata"]
        .as_array()
        .and_then(|a| a.iter().find(|e| e["name"] == name))
        .map(|e| text(&e["value"]).trim().to_string())
        .unwrap_or_default()
}

/// Parse an API answer (`formatversion=2`).
pub(super) fn parse_page(body: &[u8]) -> Result<Page> {
    let v: Value = serde_json::from_slice(body).context("API answer is not JSON")?;
    if let Some(err) = v.get("error") {
        if err["code"] == "maxlag" {
            return Ok(Page {
                maxlag: true,
                ..Page::default()
            });
        }
        bail!("API error: {err}");
    }
    let mut page = Page::default();
    if let Some(pages) = v["query"]["pages"].as_array() {
        for p in pages {
            let Some(info) = p["imageinfo"].get(0) else {
                continue;
            };
            page.candidates.push(Candidate {
                title: text(&p["title"]),
                url: text(&info["url"]),
                size: info["size"].as_u64().unwrap_or(0),
                mime: text(&info["mime"]),
                licence_short: text(&info["extmetadata"]["LicenseShortName"]["value"]),
                artist_html: text(&info["extmetadata"]["Artist"]["value"]),
                make: exif(info, "Make"),
                model: exif(info, "Model"),
                sha1: text(&info["sha1"]),
                timestamp: text(&info["timestamp"]),
            });
        }
    }
    if let Some(c) = v["continue"].as_object() {
        page.cont = Some(
            c.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str().map_or_else(|| v.to_string(), str::to_string),
                    )
                })
                .collect(),
        );
    }
    Ok(page)
}

/// Normalised licence string for an acceptable `LicenseShortName`, else `None`.
pub(super) fn accept_licence(short: &str) -> Option<String> {
    let s = short.trim();
    if s == "CC0" || s.starts_with("CC0 ") {
        return Some("CC0-1.0".to_string());
    }
    // Fixed table of SPDX identifiers; anything else (NC, ND, combined licences such as
    // "CC BY 3.0, GFDL", unknown variants) is rejected.
    let (base, rest) = if let Some(r) = s.strip_prefix("CC BY-SA ") {
        ("CC-BY-SA", r)
    } else {
        ("CC-BY", s.strip_prefix("CC BY ")?)
    };
    let mut parts = rest.split(' ');
    let version = parts.next()?;
    let suffix = parts.next().map(str::to_ascii_uppercase);
    if parts.next().is_some() {
        return None;
    }
    let known = matches!(
        (base, version, suffix.as_deref()),
        (_, "1.0" | "2.0" | "2.5" | "3.0" | "4.0", None)
            | (
                "CC-BY",
                "3.0",
                Some("AT" | "AU" | "DE" | "NL" | "US" | "IGO")
            )
            | ("CC-BY", "2.5", Some("AU"))
            | ("CC-BY-SA", "3.0", Some("AT" | "DE" | "IGO"))
            | ("CC-BY-SA", "2.0", Some("UK"))
            | ("CC-BY-SA", "2.1", Some("JP"))
    );
    if !known {
        return None;
    }
    Some(match suffix {
        Some(x) => format!("{base}-{version}-{x}"),
        None => format!("{base}-{version}"),
    })
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&e| e <= 10).and_then(|e| {
            let ent = &rest[1..e];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => ent.strip_prefix('#').and_then(|n| {
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)
                }),
            };
            ch.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Plain text of an HTML fragment: tags removed, entities decoded, white space collapsed.
pub(super) fn strip_html(html: &str) -> String {
    let mut plain = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    decode_entities(&plain)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// ASCII slug of a title (lower case letters and digits, single `-`), at most 60 characters.
pub(super) fn slug(title: &str) -> String {
    let t = title.strip_prefix("File:").unwrap_or(title);
    let stem = t.rsplit_once('.').map_or(t, |(s, _)| s);
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(60);
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "image".to_string()
    } else {
        out
    }
}

/// Output name: slug, eight hex digits of the BLAKE3 of the exact title, `.jpg`.
pub(super) fn output_name(title: &str) -> String {
    let h = blake3::hash(title.as_bytes()).to_hex();
    format!("{}-{}.jpg", slug(title), &h.as_str()[..8])
}

/// Apply the acceptance rules; `None` when the file is rejected.
pub(super) fn accept(c: &Candidate, spec: &CommonsSpec) -> Option<ListedFile> {
    if c.mime != "image/jpeg" || c.make.is_empty() || c.model.is_empty() {
        return None;
    }
    if c.size < spec.min_bytes || c.size > spec.max_bytes {
        return None;
    }
    let licence = accept_licence(&c.licence_short)?;
    let artist = strip_html(&c.artist_html);
    if artist.is_empty() && licence != "CC0-1.0" {
        return None;
    }
    let url = c.url.split('?').next().unwrap_or(&c.url);
    if !url.starts_with("https://upload.wikimedia.org/") {
        return None;
    }
    let attribution = if artist.is_empty() {
        "unknown (Wikimedia Commons)".to_string()
    } else {
        format!("{artist} (Wikimedia Commons)")
    };
    Some(ListedFile {
        url: url.to_string(),
        path: output_name(&c.title),
        licence: Some(licence),
        attribution: Some(attribution),
        extra: BTreeMap::from([
            ("sha1".to_string(), c.sha1.clone()),
            ("timestamp".to_string(), c.timestamp.clone()),
            ("title".to_string(), c.title.clone()),
        ]),
    })
}

fn fetch_page(ctx: &Ctx<'_>, source: &Source, url: &str) -> Result<Page> {
    for attempt in 1..=MAXLAG_ATTEMPTS {
        let (body, retry_after) = ctx.api_get_response(source, url, API_LIMIT)?;
        let page = parse_page(&body).with_context(|| format!("source `{}`: {url}", source.id))?;
        if !page.maxlag {
            return Ok(page);
        }
        eprintln!("  {}: server lag, waiting (attempt {attempt})", source.id);
        ctx.downloader().maxlag_wait(attempt, retry_after);
    }
    bail!("source `{}`: the API kept answering maxlag", source.id)
}

/// Resolve the listing (only under `--update-lock`).
pub fn resolve(ctx: &mut Ctx<'_>, source: &Source, spec: &CommonsSpec) -> Result<Vec<ListedFile>> {
    let mut cont = BTreeMap::new();
    let mut out: Vec<ListedFile> = Vec::new();
    let mut seen_titles = BTreeSet::new();
    let mut seen_urls = BTreeSet::new();
    let mut pages = 0u32;
    let mut seen_conts: BTreeSet<BTreeMap<String, String>> = BTreeSet::new();
    loop {
        let page = fetch_page(ctx, source, &api_url(&spec.category, &cont))?;
        pages += 1;
        for c in &page.candidates {
            if out.len() >= spec.count {
                break;
            }
            if !seen_titles.insert(c.title.clone()) {
                continue;
            }
            if let Some(f) = accept(c, spec) {
                if seen_urls.insert(f.url.clone()) {
                    out.push(f);
                }
            }
        }
        eprintln!(
            "  {}: page {pages}, {} of {} accepted",
            source.id,
            out.len(),
            spec.count
        );
        if out.len() >= spec.count {
            break;
        }
        match page.cont {
            Some(c) => {
                ensure!(
                    seen_conts.insert(c.clone()),
                    "source `{}`: the API repeated a continuation ({c:?}); stopping",
                    source.id
                );
                ensure!(
                    pages < MAX_PAGES,
                    "source `{}`: more than {MAX_PAGES} pages without enough files",
                    source.id
                );
                cont = c;
            }
            None => break,
        }
    }
    ensure!(
        out.len() == spec.count,
        "source `{}`: the category holds only {} acceptable files, {} wanted",
        source.id,
        out.len(),
        spec.count
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::build::{build, BuildOptions};
    use super::super::fetch::fake::{fast_retry, FakeFetcher};
    use super::super::manifest::Manifest;
    use super::super::registry::Profile;
    use super::*;
    use serde_json::json;

    fn spec(count: usize) -> CommonsSpec {
        CommonsSpec {
            category: "Category:Test".into(),
            count,
            min_bytes: 100,
            max_bytes: 1000,
        }
    }

    /// The bytes the fake server serves for a title (fill byte = first letter after `File:`).
    fn content(title: &str, size: u64) -> Vec<u8> {
        vec![title.as_bytes()[5]; size as usize]
    }

    fn sha1_hex(b: &[u8]) -> String {
        use sha1::{Digest, Sha1};
        Sha1::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    fn file_url(n: &str) -> String {
        format!("https://upload.wikimedia.org/wikipedia/commons/a/ab/{n}.jpg")
    }

    /// One page entry of the API answer.
    fn entry(title: &str, size: u64, licence: &str, artist: &str, mime: &str, exif: bool) -> Value {
        let meta = if exif {
            json!([{"name":"Make","value":"Canon"},{"name":"Model","value":"EOS"}])
        } else {
            json!([{"name":"Software","value":"GIMP"}])
        };
        json!({
            "title": title,
            "imageinfo": [{
                "timestamp": "2020-01-02T03:04:05Z",
                "size": size,
                "url": format!("{}?utm_source=commons.wikimedia.org&utm_content=original",
                    file_url(title.trim_start_matches("File:").trim_end_matches(".jpg"))),
                "sha1": sha1_hex(&content(title, size)),
                "mime": mime,
                "commonmetadata": meta,
                "extmetadata": {
                    "LicenseShortName": {"value": licence},
                    "Artist": {"value": artist},
                },
            }],
        })
    }

    fn answer(entries: Vec<Value>, cont: Option<&str>) -> Vec<u8> {
        let mut v = json!({"batchcomplete": true, "query": {"pages": entries}});
        if let Some(c) = cont {
            v["continue"] = json!({"gcmcontinue": c, "continue": "gcmcontinue||"});
        }
        v.to_string().into_bytes()
    }

    #[test]
    fn licence_filter() {
        for ok in ["CC0", "CC BY 4.0", "CC BY 2.0", "CC BY 3.0 DE"] {
            assert!(accept_licence(ok).is_some(), "{ok}");
        }
        assert_eq!(accept_licence("CC0").as_deref(), Some("CC0-1.0"));
        assert_eq!(accept_licence("CC BY 4.0").as_deref(), Some("CC-BY-4.0"));
        assert_eq!(
            accept_licence("CC BY 3.0 DE").as_deref(),
            Some("CC-BY-3.0-DE")
        );
        assert_eq!(
            accept_licence("CC BY-SA 4.0").as_deref(),
            Some("CC-BY-SA-4.0")
        );
        assert_eq!(
            accept_licence("CC BY-SA 3.0 de").as_deref(),
            Some("CC-BY-SA-3.0-DE")
        );
        for bad in [
            "CC BY 3.0, GFDL",
            "CC BY-SA 4.0 or GPL",
            "CC BY 9.9",
            "CC BY 3.0 XX",
            "CC BY-SA 1.0 DE",
            "CC BY-NC 2.0",
            "CC BY-ND 2.0",
            "CC BY-NC-SA 3.0",
            "CC BY-NC-ND 4.0",
            "Public domain",
            "GFDL",
            "CC BY",
            "",
        ] {
            assert!(accept_licence(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn acceptance_rules() {
        let c = |title: &str, size, lic: &str, artist: &str, mime: &str, exif| {
            parse_page(&answer(
                vec![entry(title, size, lic, artist, mime, exif)],
                None,
            ))
            .expect("parse")
            .candidates
            .remove(0)
        };
        let s = spec(5);
        let good = c(
            "File:Good.jpg",
            500,
            "CC BY 4.0",
            "<a href=\"x\">Ann &amp; Bo</a>",
            "image/jpeg",
            true,
        );
        let f = accept(&good, &s).expect("accepted");
        assert_eq!(f.licence.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(
            f.attribution.as_deref(),
            Some("Ann & Bo (Wikimedia Commons)")
        );
        assert_eq!(f.url, file_url("Good"), "query string removed");
        assert_eq!(f.extra["sha1"], sha1_hex(&content("File:Good.jpg", 500)));
        assert_eq!(f.extra["timestamp"], "2020-01-02T03:04:05Z");
        assert!(accept(&c("File:A.jpg", 500, "CC0", "", "image/jpeg", true), &s).is_some());
        // Rejections: MIME, no camera, size window (both ends), licence, CC BY without artist.
        assert!(accept(&c("File:B.png", 500, "CC0", "x", "image/png", true), &s).is_none());
        assert!(accept(&c("File:C.jpg", 500, "CC0", "x", "image/jpeg", false), &s).is_none());
        assert!(accept(&c("File:D.jpg", 99, "CC0", "x", "image/jpeg", true), &s).is_none());
        assert!(accept(&c("File:E.jpg", 1001, "CC0", "x", "image/jpeg", true), &s).is_none());
        assert!(accept(&c("File:F.jpg", 100, "CC0", "x", "image/jpeg", true), &s).is_some());
        assert!(accept(&c("File:G.jpg", 1000, "CC0", "x", "image/jpeg", true), &s).is_some());
        assert!(accept(
            &c("File:H.jpg", 500, "CC BY-NC 4.0", "x", "image/jpeg", true),
            &s
        )
        .is_none());
        assert!(accept(
            &c("File:I.jpg", 500, "CC BY 4.0", "  ", "image/jpeg", true),
            &s
        )
        .is_none());
    }

    #[test]
    fn names_are_ascii_distinct_and_portable() {
        let a = output_name("File:Café Ünïcode (1).jpg");
        let b = output_name("File:CAFÉ ünïcode (1).jpg");
        let c = output_name("File:Sunset.jpg");
        let d = output_name("File:sunset.jpg");
        assert_ne!(a, b);
        assert_ne!(c, d, "titles that differ only in case get different hashes");
        assert!(c.starts_with("sunset-") && c.ends_with(".jpg"));
        assert!(a.starts_with("caf-n-code-1-"), "{a}");
        for n in [&a, &b, &c, &d, &output_name("File:日本語.jpg")] {
            assert!(n.is_ascii());
            assert!(
                super::super::extract::check_portable_component(n).is_ok(),
                "{n}"
            );
        }
        assert!(output_name("File:日本語.jpg").starts_with("image-"));
        let long = output_name(&format!("File:{}.jpg", "x".repeat(300)));
        assert!(long.len() < 80);
    }

    #[test]
    fn html_is_stripped() {
        assert_eq!(
            strip_html("<span>A  <b>B</b></span> &lt;x&gt; &#233;&#x41; &bogus; &"),
            "A B <x> éA &bogus; &"
        );
    }

    #[test]
    fn paging_stops_at_n_and_keeps_api_order() {
        let url1 = api_url("Category:Test", &BTreeMap::new());
        let cont = BTreeMap::from([
            ("gcmcontinue".to_string(), "file|AB C|1".to_string()),
            ("continue".to_string(), "gcmcontinue||".to_string()),
        ]);
        let url2 = api_url("Category:Test", &cont);
        assert!(url2.contains("gcmcontinue=file%7CAB%20C%7C1"));
        assert!(!url2.contains(['\\', ' ', '\n']), "{url2}");
        assert!(url2.starts_with("https://commons.wikimedia.org/w/api.php?action=query&generator=categorymembers&gcmtitle=Category%3ATest&gcmtype=file&gcmlimit=50&prop=imageinfo&iiprop=url|size|mime|sha1|timestamp|extmetadata|commonmetadata&iiextmetadatafilter=LicenseShortName|Artist&format=json&formatversion=2&maxlag=5"));
        let url3 = api_url(
            "Category:Test",
            &BTreeMap::from([
                ("gcmcontinue".to_string(), "p3".to_string()),
                ("continue".to_string(), "gcmcontinue||".to_string()),
            ]),
        );
        let fetcher = FakeFetcher::default();
        let ok = |t: &str| entry(t, 500, "CC0", "x", "image/jpeg", true);
        fetcher.files.borrow_mut().insert(
            url1,
            answer(
                vec![
                    ok("File:One.jpg"),
                    entry("File:Sa.jpg", 500, "CC BY-ND 4.0", "x", "image/jpeg", true),
                    ok("File:Two.jpg"),
                ],
                Some("file|AB C|1"),
            ),
        );
        fetcher.files.borrow_mut().insert(
            url2,
            answer(vec![ok("File:Three.jpg"), ok("File:Four.jpg")], Some("p3")),
        );
        fetcher
            .files
            .borrow_mut()
            .insert(url3, answer(vec![ok("File:Five.jpg")], None));
        let (_dir, mut ctx_env) = Env::new(&fetcher);
        let got = ctx_env.resolve(&spec(3)).expect("resolve");
        let titles: Vec<_> = got.iter().map(|f| f.extra["title"].as_str()).collect();
        assert_eq!(titles, ["File:One.jpg", "File:Two.jpg", "File:Three.jpg"]);
        assert_eq!(
            fetcher.call_count(),
            2,
            "stopped paging once enough files were accepted"
        );
        // Not enough files anywhere: an error, not a short listing.
        let err = ctx_env.resolve(&spec(9)).expect_err("short");
        assert!(format!("{err:#}").contains("only 5"), "{err:#}");
    }

    #[test]
    fn a_repeated_continuation_stops_the_paging_loop() {
        let fetcher = FakeFetcher::default();
        let cont = BTreeMap::from([
            ("gcmcontinue".to_string(), "same".to_string()),
            ("continue".to_string(), "gcmcontinue||".to_string()),
        ]);
        fetcher.files.borrow_mut().insert(
            api_url("Category:Test", &BTreeMap::new()),
            answer(vec![], Some("same")),
        );
        fetcher.files.borrow_mut().insert(
            api_url("Category:Test", &cont),
            answer(vec![], Some("same")),
        );
        let (_dir, mut e) = Env::new(&fetcher);
        let err = e.resolve(&spec(1)).expect_err("loop");
        assert!(
            format!("{err:#}").contains("repeated a continuation"),
            "{err:#}"
        );
        assert_eq!(fetcher.call_count(), 2);
    }

    #[test]
    fn a_huge_retry_after_on_a_maxlag_answer_is_refused() {
        let fetcher = FakeFetcher::default();
        fetcher.files.borrow_mut().insert(
            api_url("Category:Test", &BTreeMap::new()),
            br#"{"error":{"code":"maxlag"}}"#.to_vec(),
        );
        fetcher
            .retry_after
            .set(Some(std::time::Duration::from_secs(100_000)));
        let (_dir, mut e) = Env::new(&fetcher);
        let err = e.resolve(&spec(1)).expect_err("refused");
        assert!(format!("{err:#}").contains("Retry-After"), "{err:#}");
        assert_eq!(fetcher.call_count(), 1, "no waiting, no retry");
    }

    #[test]
    fn maxlag_is_retried() {
        let fetcher = FakeFetcher::default();
        let url = api_url("Category:Test", &BTreeMap::new());
        fetcher.files.borrow_mut().insert(
            url,
            br#"{"error":{"code":"maxlag","info":"lagged"}}"#.to_vec(),
        );
        let (_dir, mut e) = Env::new(&fetcher);
        let err = e.resolve(&spec(1)).expect_err("always lagged");
        assert!(format!("{err:#}").contains("maxlag"));
        assert_eq!(fetcher.call_count(), MAXLAG_ATTEMPTS as usize);
    }

    /// Resolver environment: an update-lock context over a fake fetcher.
    struct Env<'a> {
        ctx: Ctx<'a>,
        source: Source,
    }

    impl<'a> Env<'a> {
        fn new(fetcher: &'a FakeFetcher) -> (tempfile::TempDir, Env<'a>) {
            let dir = tempfile::tempdir().expect("tmp");
            let ctx = Ctx::for_tests(fetcher, dir.path(), true);
            let source = Source {
                id: "wc".into(),
                class: "photo-jpeg".into(),
                licence: "CC0-1.0".into(),
                origin: "t".into(),
                profiles: vec![Profile::Small],
                optional: false,
                inputs: vec![],
                spec: super::super::registry::SourceSpec::CommonsPhotos(spec(1)),
            };
            (dir, Env { ctx, source })
        }
        fn resolve(&mut self, s: &CommonsSpec) -> Result<Vec<ListedFile>> {
            resolve(&mut self.ctx, &self.source, s)
        }
    }

    const REGISTRY: &str = r#"
[[source]]
id = "wc"
class = "photo-jpeg"
licence = "CC0-1.0"
origin = "commons"
profiles = ["small"]
kind = "commons-photos"
category = "Category:Test"
count = 2
min_bytes = 5
max_bytes = 100
"#;

    #[test]
    fn normal_build_makes_no_api_call_and_the_manifest_carries_licence_and_artist() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        std::fs::write(root.join("sources.toml"), REGISTRY).expect("w");
        let fetcher = FakeFetcher::default();
        let ok = |t: &str, size| entry(t, size, "CC BY 4.0", "<i>Zoe</i>", "image/jpeg", true);
        fetcher.files.borrow_mut().insert(
            api_url("Category:Test", &BTreeMap::new()),
            answer(
                vec![
                    ok("File:Alpha.jpg", 10),
                    ok("File:Huge.jpg", 9999),
                    ok("File:Beta.jpg", 12),
                ],
                None,
            ),
        );
        fetcher
            .files
            .borrow_mut()
            .insert(file_url("Alpha"), content("File:Alpha.jpg", 10));
        fetcher
            .files
            .borrow_mut()
            .insert(file_url("Beta"), content("File:Beta.jpg", 12));
        let opts = |out: &str, update_lock| BuildOptions {
            profile: Profile::Small,
            out: root.join(out),
            cache: root.join("cache"),
            only: vec![],
            update_lock,
            sources_path: root.join("sources.toml"),
            lock_path: root.join("corpus.lock"),
            retry: fast_retry(),
            git_program: None,
            allow_unavailable: false,
            ffmpeg_program: None,
        };
        build(&opts("o1", true), &fetcher).expect("pin");
        let api = |f: &FakeFetcher| {
            f.calls
                .borrow()
                .iter()
                .filter(|u| u.contains("api.php"))
                .count()
        };
        assert_eq!(api(&fetcher), 1);
        fetcher.calls.borrow_mut().clear();
        // Remove the API answer entirely: a normal build must not need it.
        fetcher
            .files
            .borrow_mut()
            .remove(&api_url("Category:Test", &BTreeMap::new()));
        let mut normal = opts("o2", false);
        normal.cache = root.join("cache2");
        build(&normal, &fetcher).expect("normal build");
        assert_eq!(api(&fetcher), 0, "a normal build makes no API call");
        assert_eq!(
            fetcher.call_count(),
            2,
            "only the two pinned files are fetched"
        );
        let m: Manifest = serde_json::from_str(
            &std::fs::read_to_string(root.join("o2/manifest.json")).expect("m"),
        )
        .expect("json");
        let files = &m.classes["photo-jpeg"].files;
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.licence == "CC-BY-4.0"));
        let lock = std::fs::read_to_string(root.join("corpus.lock")).expect("lock");
        assert!(lock.contains("\"attribution\": \"Zoe (Wikimedia Commons)\""));
        assert!(lock.contains(&format!(
            "\"sha1\": \"{}\"",
            sha1_hex(&content("File:Alpha.jpg", 10))
        )));
        assert!(root
            .join("o2/photo-jpeg/wc")
            .join(output_name("File:Alpha.jpg"))
            .is_file());
    }

    fn opts_for(root: &std::path::Path, out: &str, cache: &str, update_lock: bool) -> BuildOptions {
        BuildOptions {
            profile: Profile::Small,
            out: root.join(out),
            cache: root.join(cache),
            only: vec![],
            update_lock,
            sources_path: root.join("sources.toml"),
            lock_path: root.join("corpus.lock"),
            retry: fast_retry(),
            git_program: None,
            allow_unavailable: false,
            ffmpeg_program: None,
        }
    }

    fn api_with(items: Vec<Value>) -> FakeFetcher {
        let fetcher = FakeFetcher::default();
        fetcher.files.borrow_mut().insert(
            api_url("Category:Test", &BTreeMap::new()),
            answer(items, None),
        );
        fetcher
    }

    #[test]
    fn the_api_sha1_is_checked_against_the_bytes_when_pinning() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        std::fs::write(root.join("sources.toml"), REGISTRY).expect("w");
        let mut bad = entry("File:Alpha.jpg", 10, "CC0", "z", "image/jpeg", true);
        bad["imageinfo"][0]["sha1"] = json!("0000");
        let fetcher = api_with(vec![
            bad,
            entry("File:Beta.jpg", 12, "CC0", "z", "image/jpeg", true),
        ]);
        fetcher
            .files
            .borrow_mut()
            .insert(file_url("Alpha"), content("File:Alpha.jpg", 10));
        fetcher
            .files
            .borrow_mut()
            .insert(file_url("Beta"), content("File:Beta.jpg", 12));
        let err = build(&opts_for(root, "o", "cache", true), &fetcher).expect_err("sha1");
        assert!(format!("{err:#}").contains("SHA-1"), "{err:#}");
    }

    fn allowing(mut o: BuildOptions) -> BuildOptions {
        o.allow_unavailable = true;
        o
    }

    /// Pin two files, then let `change` alter the upstream.
    fn pinned(root: &std::path::Path) -> FakeFetcher {
        std::fs::write(root.join("sources.toml"), REGISTRY).expect("w");
        let fetcher = api_with(vec![
            entry("File:Alpha.jpg", 10, "CC0", "z", "image/jpeg", true),
            entry("File:Beta.jpg", 12, "CC0", "z", "image/jpeg", true),
        ]);
        for (t, n) in [("Alpha", 10), ("Beta", 12)] {
            fetcher
                .files
                .borrow_mut()
                .insert(file_url(t), content(&format!("File:{t}.jpg"), n));
        }
        build(&opts_for(root, "o1", "cache", true), &fetcher).expect("pin");
        fetcher
    }

    #[test]
    fn gone_and_changed_files_fail_by_default_and_are_left_out_with_the_flag() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        let fetcher = pinned(root);
        // Upstream: Alpha is gone (404), Beta is replaced by other bytes.
        fetcher.files.borrow_mut().remove(&file_url("Alpha"));
        fetcher
            .files
            .borrow_mut()
            .insert(file_url("Beta"), vec![9; 12]);
        // Default: fails, names both files and both ways forward.
        let err = build(&opts_for(root, "o2", "cache2", false), &fetcher).expect_err("fails");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Alpha.jpg") && msg.contains("Beta.jpg"),
            "{msg}"
        );
        assert!(
            msg.contains("--update-lock") && msg.contains("--allow-unavailable"),
            "{msg}"
        );
        assert!(
            !root.join("o2/manifest.json").exists(),
            "no manifest after a failure"
        );
        // With the flag: continues, records both.
        let report =
            build(&allowing(opts_for(root, "o3", "cache3", false)), &fetcher).expect("allowed");
        assert_eq!(report.unavailable.len(), 2, "{:?}", report.unavailable);
        assert!(report.unavailable[0].reason.contains("404"));
        assert!(report.unavailable[1].reason.contains("does not match"));
        assert_eq!(report.files, 0);
        let info = std::fs::read_to_string(root.join("o3/build-info.json")).expect("info");
        assert!(info.contains("\"unavailable\""), "{info}");
        // Pinning stays fatal, flag or not.
        assert!(build(&allowing(opts_for(root, "o4", "cache4", true)), &fetcher).is_err());
    }

    #[test]
    fn one_gone_file_with_the_flag_keeps_the_rest() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        let fetcher = pinned(root);
        fetcher.files.borrow_mut().remove(&file_url("Alpha"));
        let report =
            build(&allowing(opts_for(root, "o2", "cache2", false)), &fetcher).expect("allowed");
        assert_eq!(report.unavailable.len(), 1);
        assert_eq!(report.files, 1, "the other file is still built");
    }

    #[test]
    fn transient_failures_stay_fatal_even_with_the_flag() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        let fetcher = pinned(root);
        fetcher.fail_first.set(1000);
        let err = build(&allowing(opts_for(root, "o2", "cache2", false)), &fetcher)
            .expect_err("network trouble is not unavailability");
        assert!(format!("{err:#}").contains("gave up"), "{err:#}");
        // A Retry-After beyond the cap is a refusal, also fatal.
        fetcher
            .retry_after
            .set(Some(std::time::Duration::from_secs(5000)));
        let err = build(&allowing(opts_for(root, "o3", "cache3", false)), &fetcher)
            .expect_err("refused wait");
        assert!(format!("{err:#}").contains("Retry-After"), "{err:#}");
    }

    #[test]
    fn a_static_files_source_stays_fatal_even_with_the_flag() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        std::fs::write(
            root.join("sources.toml"),
            "[[source]]\nid = \"f\"\nclass = \"c\"\nlicence = \"MIT\"\norigin = \"t\"\n\
             profiles = [\"small\"]\nkind = \"files\"\n\
             files = [{ url = \"https://example.org/a.bin\" }]\n",
        )
        .expect("w");
        let fetcher = FakeFetcher::with("https://example.org/a.bin", b"data".to_vec());
        build(&opts_for(root, "o1", "cache", true), &fetcher).expect("pin");
        fetcher.files.borrow_mut().clear();
        let err = build(&allowing(opts_for(root, "o2", "cache2", false)), &fetcher)
            .expect_err("static sources never tolerate missing files");
        assert!(format!("{err:#}").contains("404"), "{err:#}");
    }
}
