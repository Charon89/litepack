//! The model's selection kinds, states, titles and requests (E6-2), on every OS.
#![allow(clippy::expect_used, clippy::indexing_slicing)]

use crate::model::*;

fn file(name: &str) -> Option<Entry> {
    Some(Entry {
        name: name.into(),
        is_dir: false,
        parent: "C:\\data\\Photos".into(),
    })
}

fn dir(name: &str) -> Option<Entry> {
    Some(Entry {
        name: name.into(),
        is_dir: true,
        parent: "C:\\data\\Photos".into(),
    })
}

fn sel(items: Vec<Option<Entry>>) -> Selection {
    classify(items.len(), items).expect("a menu")
}

fn shown(s: &Selection) -> Vec<Verb> {
    Verb::ALL
        .into_iter()
        .filter(|v| visible(s.kind, *v))
        .collect()
}

#[test]
fn every_kind_shows_the_designs_verbs() {
    use Verb::*;
    let cases: Vec<(Vec<Option<Entry>>, Kind, Vec<Verb>)> = vec![
        (
            vec![file("a.txt"), file("b.png")],
            Kind::F,
            vec![AddToArchive, AddToNamed],
        ),
        (vec![dir("src")], Kind::D, vec![AddToArchive, AddToNamed]),
        (vec![], Kind::B, vec![AddToArchive]),
        (
            vec![file("x.lpk")],
            Kind::L,
            vec![AddToArchive, ExtractHere, ExtractTo, Open, Test],
        ),
        (
            vec![file("x.7z")],
            Kind::A,
            vec![AddToArchive, ExtractHere, ExtractTo, Open, Test, Convert],
        ),
        (
            vec![file("x.7z"), file("y.lpk")],
            Kind::S,
            vec![AddToArchive, ExtractHere, ExtractTo, Test],
        ),
        (
            vec![file("x.7z"), file("y.txt")],
            Kind::M,
            vec![AddToArchive, AddToNamed],
        ),
        (
            vec![file("a.txt"), dir("d")],
            Kind::M,
            vec![AddToArchive, AddToNamed],
        ),
        (
            vec![dir("d"), file("y.lpk")],
            Kind::M,
            vec![AddToArchive, AddToNamed],
        ),
    ];
    for (items, kind, verbs) in cases {
        let s = sel(items);
        assert_eq!(s.kind, kind);
        assert_eq!(shown(&s), verbs, "{kind:?}");
    }
}

#[test]
fn a_zip_is_an_archive_even_though_explorer_calls_it_a_folder() {
    // The shell side reports a ZIP as is_dir = false (folder and stream), so the extension
    // decides; a real folder named like an archive stays a folder.
    assert_eq!(sel(vec![file("a.zip")]).kind, Kind::A);
    assert_eq!(sel(vec![dir("backup.7z")]).kind, Kind::D);
}

#[test]
fn volumes_of_one_set_are_one_archive() {
    let one = sel(vec![
        file("big.7z.001"),
        file("big.7z.002"),
        file("big.7z.003"),
    ]);
    assert_eq!(one.kind, Kind::A);
    assert_eq!(one.archives, vec![0]);
    assert_eq!(one.name, "big");
    let part = sel(vec![file("b.part2.lzma"), file("b.part1.lzma")]);
    assert_eq!((part.kind, part.archives.clone()), (Kind::A, vec![1]));
    assert_eq!(part.name, "b");
    let two = sel(vec![file("a.7z.001"), file("a.7z.002"), file("b.7z.001")]);
    assert_eq!((two.kind, two.archives.clone()), (Kind::S, vec![0, 2]));
    // Digits after something that is no archive extension are no volume.
    assert_eq!(sel(vec![file("log.txt.001")]).kind, Kind::F);
    assert_eq!(archive_stem("a.7z.12"), None);
}

#[test]
fn names_follow_the_design() {
    assert_eq!(sel(vec![file("photo.final.jpg")]).name, "photo.final");
    assert_eq!(sel(vec![file(".gitignore")]).name, ".gitignore");
    assert_eq!(sel(vec![dir("My.Folder")]).name, "My.Folder");
    assert_eq!(sel(vec![file("src.tar.gz")]).name, "src");
    assert_eq!(sel(vec![file("SRC.TAR.XZ")]).name, "SRC");
    assert_eq!(sel(vec![file("a.tgz")]).name, "a");
    assert_eq!(sel(vec![file("a.txt"), file("b.txt")]).name, "Photos");
    // A drive's root has no name; forbidden characters never reach `--name`.
    let root = Entry {
        name: "a.txt".into(),
        is_dir: false,
        parent: "C:\\".into(),
    };
    assert_eq!(sel(vec![Some(root.clone()), Some(root)]).name, "C_");
    assert_eq!(sel(vec![file("we:ird?.txt")]).name, "we_ird_");
    assert_eq!(sel(vec![file("...")]).name, FALLBACK_NAME);
}

#[test]
fn titles_are_escaped_and_bounded() {
    let s = sel(vec![dir("Fish & Chips")]);
    assert_eq!(title(Verb::AddToNamed, &s), "Add to \"Fish && Chips.lpk\"");
    assert_eq!(
        title(Verb::ExtractTo, &sel(vec![file("a.zip")])),
        "Extract to \"a\\\""
    );
    let bidi = sel(vec![file("x\u{202E}gpj.exe")]);
    assert_eq!(
        title(Verb::AddToNamed, &bidi),
        "Add to \"x\\u{202E}gpj.lpk\""
    );
    let long = "n".repeat(60);
    let s = sel(vec![dir(&long)]);
    assert_eq!(s.name, long, "the verb carries the full name");
    assert_eq!(
        title(Verb::AddToNamed, &s),
        format!(
            "Add to \"{}\u{2026}{}.lpk\"",
            "n".repeat(19),
            "n".repeat(20)
        )
    );
    let several = sel(vec![file("a.7z"), file("b.7z")]);
    assert_eq!(title(Verb::ExtractTo, &several), "Extract to \"*\\\"");
    assert_eq!(title(Verb::Open, &several), "Open");
}

#[test]
fn arbitrary_names_never_panic_and_titles_stay_bounded() {
    let pool: Vec<char> = "ab.&\\/:*?\u{0}\u{7}\u{202E}\u{200F}\u{2066}\u{1F600}\u{10FFFF}z7 _-"
        .chars()
        .collect();
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..3000 {
        let n_items = usize::try_from(next() % 4).unwrap_or(0) + 1;
        let items: Vec<Option<Entry>> = (0..n_items)
            .map(|_| {
                let len = usize::try_from(next() % 120).unwrap_or(0);
                let name: String = (0..len)
                    .map(|_| pool[usize::try_from(next() % 1000).unwrap_or(0) % pool.len()])
                    .collect();
                Some(Entry {
                    name,
                    is_dir: next() % 3 == 0,
                    parent: "C:\\x\\y".into(),
                })
            })
            .collect();
        let s = classify(items.len(), items).expect("menu");
        for v in Verb::ALL {
            assert!(title(v, &s).chars().count() <= MAX_TITLE_CHARS);
            if let Some(Request { name: Some(n), .. }) = request(v, &s) {
                assert!(n.len() > 4 && !n.contains(['/', '\\', ':']) && n.trim() == n);
            }
        }
    }
}

#[test]
fn a_pathless_item_hides_the_menu_and_the_bound_stops_the_scan() {
    assert_eq!(classify(2, vec![file("a"), None]), None);
    let mut read = 0usize;
    let lazy = std::iter::from_fn(|| {
        read += 1;
        Some(file("a.txt"))
    });
    let s = classify(MAX_ITEMS + 1, lazy).expect("menu");
    assert_eq!(s.kind, Kind::Over);
    assert_eq!(read, 0, "past the bound no item is read");
    assert_eq!(shown(&s), vec![Verb::AddToArchive]);
    let s = classify(MAX_ITEMS, (0..MAX_ITEMS).map(|_| file("a.txt"))).expect("menu");
    assert_eq!(s.kind, Kind::F);
}

#[test]
fn requests_per_verb() {
    let a = sel(vec![file("x.7z")]);
    let r = request(Verb::Convert, &a).expect("visible");
    assert_eq!((r.flag, r.items, r.name), ("--convert", vec![0], None));
    let f = sel(vec![file("a.txt"), dir("d")]);
    let r = request(Verb::AddToNamed, &f).expect("visible");
    assert_eq!(
        (r.flag, r.items, r.name),
        ("--create", vec![0, 1], Some("Photos.lpk".into()))
    );
    assert_eq!(request(Verb::Open, &f), None);
    let b = sel(vec![]);
    let r = request(Verb::AddToArchive, &b).expect("visible");
    assert!(r.background && r.items.is_empty());
    let s = sel(vec![file("a.7z"), file("b.7z")]);
    assert_eq!(request(Verb::Test, &s).expect("visible").items, vec![0, 1]);
    assert_eq!(
        request(Verb::AddToArchive, &s).expect("visible").items,
        vec![0, 1]
    );
}
