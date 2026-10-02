//! `kind = "arxiv-papers"`: CC-BY PDFs chosen from arXiv's OAI-PMH feed.
//!
//! A list kind: the resolver runs only under `--update-lock`. It lists every record whose
//! datestamp lies in the registry's window (`verb=ListRecords`, `metadataPrefix=arXivRaw`,
//! following `resumptionToken`), keeps those licensed `http://creativecommons.org/licenses/by/4.0/`,
//! sorts them by identifier and takes the first N. Each PDF is the newest version the record
//! lists (`https://export.arxiv.org/pdf/<id>v<n>`).
//!
//! The arXiv `arXivRaw` elements used: `header/identifier`, `header@status`,
//! `arXivRaw/{id, version@version, title, authors, license}` and `resumptionToken`.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

use super::build::{Ctx, ListedFile};
use super::commons::percent_encode;
use super::registry::{ArxivSpec, Source};

/// The licence URL accepted (CC BY 4.0).
pub const CC_BY_4: &str = "http://creativecommons.org/licenses/by/4.0/";
const OAI: &str = "https://oaipmh.arxiv.org/oai";
const PDF_HOST: &str = "https://export.arxiv.org/";
const API_LIMIT: u64 = 256 << 20;

/// One `arXivRaw` record.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub deleted: bool,
    pub id: String,
    pub license: String,
    pub authors: String,
    pub title: String,
    /// Version labels as listed (`v1`, `v2`, ...).
    pub versions: Vec<String>,
}

/// One `ListRecords` answer.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ListPage {
    pub records: Vec<Record>,
    pub token: Option<String>,
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == key)
        .map(|a| a.value.to_string())
}

/// Parse one `ListRecords` answer.
pub(super) fn parse_list(xml: &[u8]) -> Result<ListPage> {
    let mut reader = Reader::from_reader(xml);
    let mut page = ListPage::default();
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut cur: Option<Record> = None;
    let mut header_deleted = false;
    loop {
        match reader.read_event().context("invalid XML")? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_string();
                text.clear();
                match name.as_str() {
                    "record" => cur = Some(Record::default()),
                    "header" => header_deleted = attr(&e, "status").as_deref() == Some("deleted"),
                    "version" => {
                        if let (Some(r), Some(v)) = (cur.as_mut(), attr(&e, "version")) {
                            r.versions.push(v);
                        }
                    }
                    "error" => {
                        let code = attr(&e, "code").unwrap_or_default();
                        if code != "noRecordsMatch" {
                            bail!("OAI-PMH error `{code}`");
                        }
                    }
                    _ => {}
                }
                path.push(name);
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(t) => text.push_str(&t.xml10_content()),
            Event::GeneralRef(r) => {
                if let Some(c) = r.resolve_char_ref().context("bad character reference")? {
                    text.push(c);
                } else {
                    match &*r {
                        "amp" => text.push('&'),
                        "lt" => text.push('<'),
                        "gt" => text.push('>'),
                        "quot" => text.push('"'),
                        "apos" => text.push('\''),
                        other => bail!("unknown entity &{other};"),
                    }
                }
            }
            Event::End(_) => {
                let name = path.pop().unwrap_or_default();
                let parent = path.last().map(String::as_str).unwrap_or("");
                match (name.as_str(), parent) {
                    ("record", _) => {
                        if let Some(mut r) = cur.take() {
                            r.deleted = header_deleted;
                            page.records.push(r);
                        }
                    }
                    ("id", "arXivRaw") => {
                        if let Some(r) = cur.as_mut() {
                            r.id = collapse(&text);
                        }
                    }
                    ("license", "arXivRaw") => {
                        if let Some(r) = cur.as_mut() {
                            r.license = collapse(&text);
                        }
                    }
                    ("authors", "arXivRaw") => {
                        if let Some(r) = cur.as_mut() {
                            r.authors = collapse(&text);
                        }
                    }
                    ("title", "arXivRaw") => {
                        if let Some(r) = cur.as_mut() {
                            r.title = collapse(&text);
                        }
                    }
                    ("resumptionToken", _) => {
                        let t = text.trim();
                        page.token = (!t.is_empty()).then(|| t.to_string());
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(page)
}

/// Number of a version label (`v12` -> 12).
fn version_number(v: &str) -> Option<u32> {
    v.strip_prefix('v')?.parse().ok()
}

/// The newest version a record lists.
pub(super) fn newest_version(r: &Record) -> Option<u32> {
    r.versions.iter().filter_map(|v| version_number(v)).max()
}

/// Turn accepted records into listed files: CC BY 4.0 only, deleted records and records
/// without a version dropped, sorted by identifier, first `count`.
pub(super) fn select(records: &[Record], count: usize) -> Vec<ListedFile> {
    let mut accepted: BTreeMap<&str, (&Record, u32)> = BTreeMap::new();
    for r in records {
        if r.deleted || r.id.is_empty() || r.license != CC_BY_4 {
            continue;
        }
        if let Some(n) = newest_version(r) {
            accepted.entry(r.id.as_str()).or_insert((r, n));
        }
    }
    accepted
        .into_iter()
        .take(count)
        .map(|(id, (r, n))| ListedFile {
            url: format!("{PDF_HOST}pdf/{id}v{n}"),
            path: format!("arxiv-{}v{n}.pdf", id.replace('/', "_")),
            licence: Some("CC-BY-4.0".to_string()),
            attribution: Some(if r.authors.is_empty() {
                "unknown (arXiv)".to_string()
            } else {
                format!("{} (arXiv:{id})", r.authors)
            }),
            extra: BTreeMap::from([
                ("id".to_string(), id.to_string()),
                ("title".to_string(), r.title.clone()),
                ("version".to_string(), format!("v{n}")),
            ]),
        })
        .collect()
}

/// URL of the first page of the window.
pub(super) fn first_url(spec: &ArxivSpec) -> String {
    let mut u = format!(
        "{OAI}?verb=ListRecords&metadataPrefix=arXivRaw&from={}&until={}",
        spec.from, spec.until
    );
    if let Some(set) = &spec.set {
        u.push_str("&set=");
        u.push_str(&percent_encode(set));
    }
    u
}

/// URL of a later page.
pub(super) fn token_url(token: &str) -> String {
    format!(
        "{OAI}?verb=ListRecords&resumptionToken={}",
        percent_encode(token)
    )
}

/// Resolve the listing (only under `--update-lock`).
pub fn resolve(ctx: &mut Ctx<'_>, source: &Source, spec: &ArxivSpec) -> Result<Vec<ListedFile>> {
    let mut url = first_url(spec);
    let mut all: Vec<Record> = Vec::new();
    let mut pages = 0u32;
    loop {
        let body = ctx.api_get(source, &url, API_LIMIT)?;
        let page = parse_list(&body).with_context(|| format!("source `{}`: {url}", source.id))?;
        pages += 1;
        all.extend(page.records);
        eprintln!(
            "  {}: page {pages}, {} records so far",
            source.id,
            all.len()
        );
        match page.token {
            Some(t) => url = token_url(&t),
            None => break,
        }
    }
    let listing = select(&all, spec.count);
    ensure!(
        listing.len() == spec.count,
        "source `{}`: the window holds only {} CC-BY-4.0 papers, {} wanted",
        source.id,
        listing.len(),
        spec.count
    );
    // The PDFs come from another host of the same operator, which allows one request per
    // interval across both: wait out the interval before the first download.
    ctx.downloader().settle(&format!("{PDF_HOST}pdf/"));
    Ok(listing)
}

#[cfg(test)]
mod tests {
    use super::super::fetch::fake::FakeFetcher;
    use super::super::registry::{Profile, SourceSpec};
    use super::*;

    fn record(id: &str, license: &str, versions: &[&str], extra_header: &str) -> String {
        let vs: String = versions
            .iter()
            .map(|v| format!("<version version=\"{v}\"><date>Mon</date><size>1kb</size></version>"))
            .collect();
        format!(
            r#"<record><header status="{extra_header}"><identifier>oai:arXiv.org:{id}</identifier></header>
<metadata><arXivRaw xmlns="http://arxiv.org/OAI/arXivRaw/"><id>{id}</id>{vs}
<title>Deep &amp; wide
  nets</title><authors>Ana M&#233;ndez, B. Cho</authors><license>{license}</license>
<abstract>ignored &lt;b&gt;</abstract></arXivRaw></metadata></record>"#
        )
    }

    fn page(records: &[String], token: Option<&str>) -> Vec<u8> {
        let tok = token.map_or("<resumptionToken cursor=\"0\"/>".to_string(), |t| {
            format!("<resumptionToken cursor=\"0\" completeListSize=\"9\">{t}</resumptionToken>")
        });
        format!(
            r#"<?xml version="1.0"?><OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/"><ListRecords>{}{tok}</ListRecords></OAI-PMH>"#,
            records.concat()
        )
        .into_bytes()
    }

    fn spec(count: usize) -> ArxivSpec {
        ArxivSpec {
            from: "2025-03-03".into(),
            until: "2025-03-04".into(),
            set: Some("cs".into()),
            count,
        }
    }

    #[test]
    fn parses_records_entities_and_versions() {
        let xml = page(
            &[record("2503.00002", CC_BY_4, &["v1", "v10", "v9"], "")],
            Some("tok/1 2"),
        );
        let p = parse_list(&xml).expect("parse");
        assert_eq!(p.token.as_deref(), Some("tok/1 2"));
        let r = &p.records[0];
        assert_eq!(r.id, "2503.00002");
        assert_eq!(r.title, "Deep & wide nets");
        assert_eq!(r.authors, "Ana Méndez, B. Cho");
        assert_eq!(r.license, CC_BY_4);
        assert!(!r.deleted);
        assert_eq!(newest_version(r), Some(10), "numeric, not lexical");
        // An empty resumptionToken ends the list.
        assert_eq!(parse_list(&page(&[], None)).expect("p").token, None);
        assert!(parse_list(
            br#"<OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/"><error code="badArgument">x</error></OAI-PMH>"#
        )
        .is_err());
        assert!(parse_list(
            br#"<OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/"><error code="noRecordsMatch">x</error></OAI-PMH>"#
        )
        .expect("empty window")
        .records
        .is_empty());
    }

    #[test]
    fn selection_filters_sorts_and_takes_the_first_n() {
        let sa = "http://creativecommons.org/licenses/by-sa/4.0/";
        let nc = "http://creativecommons.org/licenses/by-nc-sa/4.0/";
        let zero = "http://creativecommons.org/publicdomain/zero/1.0/";
        let old = "http://creativecommons.org/licenses/by/3.0/";
        let recs = [
            record("2503.00009", CC_BY_4, &["v1", "v2"], ""),
            record("2503.00001", CC_BY_4, &["v3"], ""),
            record("2503.00002", sa, &["v1"], ""),
            record("2503.00003", nc, &["v1"], ""),
            record("2503.00004", zero, &["v1"], ""),
            record("2503.00005", old, &["v1"], ""),
            record("2503.00006", CC_BY_4, &["v1"], "deleted"),
            record("2503.00007", CC_BY_4, &[], ""),
            record("2503.00001", CC_BY_4, &["v3"], ""),
            record(
                "2503.00008",
                "http://arxiv.org/licenses/nonexclusive-distrib/1.0/",
                &["v1"],
                "",
            ),
        ];
        let parsed = parse_list(&page(&recs, None)).expect("parse").records;
        assert_eq!(parsed.len(), recs.len());
        let all = select(&parsed, 10);
        let ids: Vec<_> = all.iter().map(|f| f.extra["id"].as_str()).collect();
        assert_eq!(ids, ["2503.00001", "2503.00009"]);
        assert_eq!(all[0].url, "https://export.arxiv.org/pdf/2503.00001v3");
        assert_eq!(
            all[1].url, "https://export.arxiv.org/pdf/2503.00009v2",
            "newest version"
        );
        assert_eq!(all[0].path, "arxiv-2503.00001v3.pdf");
        assert_eq!(all[0].licence.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(
            all[0].attribution.as_deref(),
            Some("Ana Méndez, B. Cho (arXiv:2503.00001)")
        );
        assert_eq!(select(&parsed, 1).len(), 1);
        let old_style = Record {
            id: "hep-th/9901001".into(),
            license: CC_BY_4.into(),
            versions: vec!["v1".into()],
            ..Record::default()
        };
        assert_eq!(
            select(&[old_style], 1)[0].path,
            "arxiv-hep-th_9901001v1.pdf"
        );
    }

    #[test]
    fn resolve_follows_resumption_tokens() {
        let fetcher = FakeFetcher::default();
        fetcher.files.borrow_mut().insert(
            first_url(&spec(2)),
            page(&[record("2503.00005", CC_BY_4, &["v1"], "")], Some("a b")),
        );
        fetcher.files.borrow_mut().insert(
            token_url("a b"),
            page(&[record("2503.00002", CC_BY_4, &["v2"], "")], Some("t2")),
        );
        fetcher.files.borrow_mut().insert(
            token_url("t2"),
            page(&[record("2503.00003", CC_BY_4, &["v1"], "")], None),
        );
        assert!(first_url(&spec(2)).ends_with("&set=cs"));
        assert!(token_url("a b").ends_with("resumptionToken=a%20b"));
        let dir = tempfile::tempdir().expect("tmp");
        let mut ctx = Ctx::for_tests(&fetcher, dir.path(), true);
        let source = Source {
            id: "ax".into(),
            class: "office-pdf".into(),
            licence: "x".into(),
            origin: "x".into(),
            profiles: vec![Profile::Small],
            optional: false,
            inputs: vec![],
            spec: SourceSpec::ArxivPapers(spec(2)),
        };
        let got = resolve(&mut ctx, &source, &spec(2)).expect("resolve");
        let ids: Vec<_> = got.iter().map(|f| f.extra["id"].as_str()).collect();
        assert_eq!(
            ids,
            ["2503.00002", "2503.00003"],
            "sorted by identifier across pages"
        );
        assert_eq!(fetcher.call_count(), 3);
        assert!(
            resolve(&mut ctx, &source, &spec(5)).is_err(),
            "not enough papers"
        );
        // Without --update-lock no API call is possible.
        let mut normal = Ctx::for_tests(&fetcher, dir.path(), false);
        assert!(resolve(&mut normal, &source, &spec(2)).is_err());
    }
}
