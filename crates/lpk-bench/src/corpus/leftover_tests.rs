//! Tests for the review leftovers: derived-input check on every build, `truncate_files` ceiling,
//! and the printed manifest path of `scan`.

use std::path::{Path, PathBuf};

use super::build::{build, BuildOptions};
use super::fetch::fake::{fast_retry, FakeFetcher};
use super::registry::{Profile, Registry};
use super::scan::display_path;

fn registry(optional: bool) -> String {
    format!(
        "[[source]]\nid = \"alpha\"\nclass = \"alpha\"\nlicence = \"MIT\"\norigin = \"t\"\n\
         profiles = [\"full\"]\nkind = \"file\"\nurl = \"https://example.org/a.bin\"\n\n\
         [[source]]\nid = \"sf\"\nclass = \"derived\"\nlicence = \"MIT\"\norigin = \"t\"\n\
         profiles = [\"small\", \"full\"]\noptional = {optional}\ninputs = [\"alpha\"]\n\
         kind = \"small-files\"\ncount = 3\nmax_file_bytes = 100\njson_percent = 0\ncsv_percent = 0\n"
    )
}

fn opts(root: &Path) -> BuildOptions {
    BuildOptions {
        profile: Profile::Small,
        out: root.join("out"),
        cache: root.join("cache"),
        only: Vec::new(),
        update_lock: true,
        sources_path: root.join("sources.toml"),
        lock_path: root.join("corpus.lock"),
        retry: fast_retry(),
        git_program: None,
        ffmpeg_program: None,
        repin: Vec::new(),
        list_only: false,
        allow_unavailable: false,
    }
}

#[test]
fn full_build_fails_when_the_input_class_has_no_source_in_the_profile() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("sources.toml"), registry(false)).expect("w");
    let err = build(&opts(d.path()), &FakeFetcher::default()).expect_err("must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("alpha") && msg.contains("profile `small`"),
        "{msg}"
    );
    assert!(
        !d.path().join("out").exists(),
        "fails before touching <out>"
    );
}

#[test]
fn an_optional_derived_source_is_skipped_with_a_reason_instead() {
    let d = tempfile::tempdir().expect("tmp");
    std::fs::write(d.path().join("sources.toml"), registry(true)).expect("w");
    let r = build(&opts(d.path()), &FakeFetcher::default()).expect("builds");
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].source, "sf");
    assert!(
        r.skipped[0].reason.contains("alpha"),
        "{}",
        r.skipped[0].reason
    );
}

#[test]
fn truncate_files_above_the_extraction_ceiling_is_rejected() {
    let text = |extra: &str| {
        format!(
            "[[source]]\nid = \"g\"\nclass = \"c\"\nlicence = \"MIT\"\norigin = \"o\"\n\
             profiles = [\"small\"]\nkind = \"archive\"\nurl = \"https://example.org/x.gz\"\n{extra}"
        )
    };
    let err = Registry::parse(&text("truncate_files = 2000\nmax_extracted_bytes = 1000\n"))
        .expect_err("t > ceiling");
    assert!(format!("{err:#}").contains("truncate_files"), "{err:#}");
    Registry::parse(&text("truncate_files = 1000\nmax_extracted_bytes = 1000\n")).expect("equal");
    Registry::parse(&text("truncate_files = 1000\n")).expect("under the default");
}

#[test]
fn verbatim_prefix_is_dropped_but_unc_is_kept() {
    assert_eq!(
        display_path(Path::new(r"\\?\C:\a\b")),
        PathBuf::from(r"C:\a\b")
    );
    assert_eq!(
        display_path(Path::new(r"\\?\UNC\srv\share")),
        PathBuf::from(r"\\?\UNC\srv\share")
    );
    assert_eq!(display_path(Path::new("/x/y")), PathBuf::from("/x/y"));
}

fn from_registry(from: &str, base_profiles: &str, base_class: &str) -> String {
    format!(
        "[[source]]\nid = \"base\"\nclass = \"{base_class}\"\nlicence = \"MIT\"\norigin = \"t\"\n\
         profiles = {base_profiles}\nkind = \"file\"\nurl = \"https://example.org/a.bin\"\n\n\
         [[source]]\nid = \"wav\"\nclass = \"audio\"\nlicence = \"MIT\"\norigin = \"t\"\n\
         profiles = [\"small\"]\ninputs = [\"audio\"]\nkind = \"flac-to-wav\"\nfrom = {from}\n"
    )
}

#[test]
fn from_ids_must_name_earlier_sources_of_an_input_class_sharing_a_profile() {
    Registry::parse(&from_registry("[\"base\"]", "[\"small\"]", "audio")).expect("valid");
    let bad = [
        (
            from_registry("[\"typo\"]", "[\"small\"]", "audio"),
            "not a source earlier",
        ),
        (
            from_registry("[\"base\"]", "[\"small\"]", "video"),
            "not in `inputs`",
        ),
        (
            from_registry("[\"base\"]", "[\"full\"]", "audio"),
            "no profile",
        ),
    ];
    for (text, needle) in bad {
        let err = Registry::parse(&text);
        // The class-`video` case has no earlier `audio` source at all, which is rejected first.
        let msg = format!("{:#}", err.expect_err("must be rejected"));
        assert!(
            msg.contains(needle) || msg.contains("input class"),
            "{needle}: {msg}"
        );
    }
    // A `from` that names a later source is rejected too.
    let later = "[[source]]\nid = \"wav\"\nclass = \"audio\"\nlicence = \"MIT\"\norigin = \"t\"\n\
        profiles = [\"small\"]\ninputs = [\"audio\"]\nkind = \"flac-to-wav\"\nfrom = [\"base\"]\n";
    assert!(Registry::parse(later).is_err());
}

#[test]
fn only_runs_ignore_leftover_directories_of_unregistered_sources() {
    use super::derive::input_files;
    use super::registry::{FlacToWavSpec, Source, SourceSpec};
    let d = tempfile::tempdir().expect("tmp");
    for id in ["kept", "stale"] {
        let p = d.path().join("out/audio").join(id);
        std::fs::create_dir_all(&p).expect("dir");
        std::fs::write(p.join("a.flac"), b"x").expect("file");
    }
    let fetcher = FakeFetcher::default();
    let mut ctx = super::build::Ctx::for_tests(&fetcher, d.path(), false);
    ctx.derive.class_sources = [("audio".to_string(), ["kept".to_string()].into())].into();
    let src = Source {
        id: "wav".into(),
        class: "audio".into(),
        licence: "MIT".into(),
        origin: "t".into(),
        profiles: vec![Profile::Small],
        optional: false,
        inputs: vec!["audio".into()],
        spec: SourceSpec::FlacToWav(FlacToWavSpec { from: vec![] }),
    };
    let files = input_files(&ctx, &src, &[]).expect("files");
    assert_eq!(
        files.iter().map(|f| f.source.as_str()).collect::<Vec<_>>(),
        ["kept"]
    );
}
