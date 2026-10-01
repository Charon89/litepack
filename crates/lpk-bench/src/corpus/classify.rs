//! Class assignment for `corpus scan --private` (by magic bytes and extension).
//!
//! All rules live in two data tables, [`SIGNATURES`] (magic bytes) and [`EXTENSIONS`]
//! (file name extensions). To teach the scanner a new format, add a row to one of them.
//!
//! Resolution order in [`classify`]:
//! 1. A file inside a `.git` directory belongs to `source-git` (history objects have no
//!    meaningful extension).
//! 2. The first [`Signature`] that matches the head of the file decides. Magic wins over the
//!    extension, so a `.jpg` that starts with the PNG signature is `photo-raw-png`. A signature
//!    may refine its default class for particular extensions (a ZIP container is
//!    `archives-nested`, unless it is a `.docx`).
//! 3. Otherwise the extension decides. Rows marked `verified` name formats that have a
//!    signature above, so a file with such an extension but no matching magic (empty,
//!    truncated, an HTML error page saved as `.jpg`) is `other`. Other rows are trusted.
//! 4. Anything else is `other`.
//!
//! Only base classes of `docs/CORPUS.md` are assigned; derived classes (`photo-jpeg-edited`,
//! `office-versions`, `backup-v*`, `small-files`, `encrypted-random`) come from the builder.

pub const PHOTO_JPEG: &str = "photo-jpeg";
pub const PHOTO_RAW_PNG: &str = "photo-raw-png";
pub const OFFICE_PDF: &str = "office-pdf";
pub const SOURCE_GIT: &str = "source-git";
pub const SOFTWARE_INSTALLED: &str = "software-installed";
pub const GAME_ASSETS: &str = "game-assets";
pub const LOGS_TEXT: &str = "logs-text";
pub const TEXT_PROSE: &str = "text-prose";
pub const AUDIO: &str = "audio";
pub const VIDEO: &str = "video";
pub const VM_IMAGE: &str = "vm-image";
pub const MODEL_WEIGHTS: &str = "model-weights";
pub const ARCHIVES_NESTED: &str = "archives-nested";
/// Files that match no rule.
pub const OTHER: &str = "other";

/// A magic-byte rule. `checks` must all match (offset, bytes).
#[derive(Clone, Copy)]
pub struct Signature {
    /// Label for readers and test failures.
    #[cfg_attr(not(test), allow(dead_code))]
    pub name: &'static str,
    pub checks: &'static [(usize, &'static [u8])],
    pub class: &'static str,
    /// If non-empty, the signature only applies to these extensions (for short magics).
    pub only_ext: &'static [&'static str],
    /// Extensions that override `class` when this signature matched.
    pub refine: &'static [(&'static [&'static str], &'static str)],
}

const fn sig(
    name: &'static str,
    checks: &'static [(usize, &'static [u8])],
    class: &'static str,
) -> Signature {
    Signature {
        name,
        checks,
        class,
        only_ext: &[],
        refine: &[],
    }
}

impl Signature {
    const fn only(mut self, exts: &'static [&'static str]) -> Signature {
        self.only_ext = exts;
        self
    }

    const fn refine(
        mut self,
        rules: &'static [(&'static [&'static str], &'static str)],
    ) -> Signature {
        self.refine = rules;
        self
    }

    fn matches(&self, head: &[u8], ext: &str) -> bool {
        (self.only_ext.is_empty() || self.only_ext.contains(&ext))
            && self.checks.iter().all(|(off, bytes)| {
                head.get(*off..*off + bytes.len())
                    .is_some_and(|h| h == *bytes)
            })
    }

    fn class_for(&self, ext: &str) -> &'static str {
        self.refine
            .iter()
            .find(|(exts, _)| exts.contains(&ext))
            .map_or(self.class, |(_, class)| class)
    }
}

const OFFICE_ZIP: &[&str] = &[
    "docx", "docm", "dotx", "xlsx", "xlsm", "pptx", "pptm", "ppsx", "odt", "ods", "odp",
];

/// Magic-byte rules, first match wins. More specific rules come before general ones.
pub const SIGNATURES: &[Signature] = &[
    sig("jpeg", &[(0, b"\xFF\xD8\xFF")], PHOTO_JPEG),
    sig("png", &[(0, b"\x89PNG\r\n\x1A\n")], PHOTO_RAW_PNG),
    sig("tiff-le", &[(0, b"II*\0")], PHOTO_RAW_PNG),
    sig("tiff-be", &[(0, b"MM\0*")], PHOTO_RAW_PNG),
    sig("bmp", &[(0, b"BM")], PHOTO_RAW_PNG).only(&["bmp"]),
    sig("pdf", &[(0, b"%PDF-")], OFFICE_PDF),
    sig(
        "ole2",
        &[(0, b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1")],
        OFFICE_PDF,
    )
    .refine(&[(&["msi", "msp", "mst"], SOFTWARE_INSTALLED)]),
    sig("zip", &[(0, b"PK\x03\x04")], ARCHIVES_NESTED)
        .refine(&[(OFFICE_ZIP, OFFICE_PDF), (&["pt", "pth"], MODEL_WEIGHTS)]),
    sig("zip-empty", &[(0, b"PK\x05\x06")], ARCHIVES_NESTED),
    sig("7z", &[(0, b"7z\xBC\xAF\x27\x1C")], ARCHIVES_NESTED),
    sig("rar", &[(0, b"Rar!\x1A\x07")], ARCHIVES_NESTED),
    sig("gzip", &[(0, b"\x1F\x8B")], ARCHIVES_NESTED),
    sig("xz", &[(0, b"\xFD7zXZ\0")], ARCHIVES_NESTED),
    sig("zstd", &[(0, b"\x28\xB5\x2F\xFD")], ARCHIVES_NESTED),
    sig("bzip2", &[(0, b"BZh")], ARCHIVES_NESTED).only(&["bz2", "tbz", "tbz2"]),
    sig("tar", &[(257, b"ustar")], ARCHIVES_NESTED),
    sig("cab", &[(0, b"MSCF")], SOFTWARE_INSTALLED),
    sig("pe", &[(0, b"MZ")], SOFTWARE_INSTALLED)
        .only(&["exe", "dll", "sys", "ocx", "scr", "efi", "cpl", "drv"]),
    sig("elf", &[(0, b"\x7FELF")], SOFTWARE_INSTALLED),
    sig("macho-64", &[(0, b"\xCF\xFA\xED\xFE")], SOFTWARE_INSTALLED),
    sig("macho-32", &[(0, b"\xCE\xFA\xED\xFE")], SOFTWARE_INSTALLED),
    sig("macho-fat", &[(0, b"\xCA\xFE\xBA\xBE")], SOFTWARE_INSTALLED).only(&["dylib", "o", "a"]),
    sig("wav", &[(0, b"RIFF"), (8, b"WAVE")], AUDIO),
    sig("avi", &[(0, b"RIFF"), (8, b"AVI ")], VIDEO),
    sig("flac", &[(0, b"fLaC")], AUDIO),
    sig("mp3-id3", &[(0, b"ID3")], AUDIO),
    sig("ogg", &[(0, b"OggS")], AUDIO).refine(&[(&["ogv"], VIDEO)]),
    sig("mp4", &[(4, b"ftyp")], VIDEO).refine(&[(&["m4a", "aac"], AUDIO)]),
    sig("matroska", &[(0, b"\x1A\x45\xDF\xA3")], VIDEO).refine(&[(&["mka"], AUDIO)]),
    sig("qcow2", &[(0, b"QFI\xFB")], VM_IMAGE),
    sig("vmdk", &[(0, b"KDMV")], VM_IMAGE),
    sig("vhd", &[(0, b"conectix")], VM_IMAGE),
    sig("vhdx", &[(0, b"vhdxfile")], VM_IMAGE),
    sig(
        "vdi",
        &[(0, b"<<< Oracle VM VirtualBox Disk Image >>>")],
        VM_IMAGE,
    ),
    sig("iso9660", &[(0x8001, b"CD001")], VM_IMAGE),
    sig("gguf", &[(0, b"GGUF")], MODEL_WEIGHTS),
];

/// An extension rule: every extension in `exts` maps to `class`.
pub struct ExtensionRule {
    pub class: &'static str,
    /// True if the format has a signature in [`SIGNATURES`]: an extension match without the
    /// magic is then not trusted (the file is `other`).
    pub verified: bool,
    pub exts: &'static [&'static str],
}

const fn ext(class: &'static str, verified: bool, exts: &'static [&'static str]) -> ExtensionRule {
    ExtensionRule {
        class,
        verified,
        exts,
    }
}

/// Extension rules (lower-case, without the dot).
pub const EXTENSIONS: &[ExtensionRule] = &[
    ext(PHOTO_JPEG, true, &["jpg", "jpeg", "jpe", "jfif"]),
    ext(PHOTO_RAW_PNG, true, &["png", "bmp", "tif", "tiff"]),
    ext(
        PHOTO_RAW_PNG,
        false,
        &["dng", "nef", "cr2", "cr3", "arw", "raf", "orf", "rw2"],
    ),
    ext(
        OFFICE_PDF,
        true,
        &[
            "pdf", "doc", "xls", "ppt", "docx", "docm", "dotx", "xlsx", "xlsm", "pptx", "pptm",
            "ppsx", "odt", "ods", "odp",
        ],
    ),
    ext(OFFICE_PDF, false, &["rtf"]),
    ext(
        SOURCE_GIT,
        false,
        &[
            "rs", "c", "h", "cc", "cpp", "cxx", "hpp", "cs", "java", "kt", "py", "js", "mjs", "ts",
            "tsx", "jsx", "go", "rb", "php", "swift", "sh", "ps1", "bat", "toml", "yaml", "yml",
            "cmake", "mk", "gradle", "lua", "sql",
        ],
    ),
    ext(
        LOGS_TEXT,
        false,
        &["log", "csv", "tsv", "json", "jsonl", "ndjson"],
    ),
    ext(
        TEXT_PROSE,
        false,
        &["txt", "md", "rst", "html", "htm", "xml", "tex"],
    ),
    ext(
        AUDIO,
        false,
        &["wav", "flac", "mp3", "ogg", "opus", "m4a", "aac", "wma"],
    ),
    ext(
        VIDEO,
        false,
        &[
            "mp4", "mkv", "webm", "avi", "mov", "m4v", "wmv", "flv", "mts", "m2ts",
        ],
    ),
    ext(
        VM_IMAGE,
        false,
        &["vmdk", "vdi", "vhd", "vhdx", "qcow2", "iso"],
    ),
    ext(
        MODEL_WEIGHTS,
        false,
        &["safetensors", "gguf", "pt", "pth", "onnx", "ckpt"],
    ),
    ext(
        SOFTWARE_INSTALLED,
        false,
        &[
            "exe", "dll", "sys", "ocx", "msi", "msp", "cab", "so", "dylib", "lib", "pdb",
        ],
    ),
    ext(
        GAME_ASSETS,
        false,
        &[
            "pak", "unity3d", "assets", "fbx", "obj", "dds", "tga", "blend", "glb", "gltf", "bsp",
            "wad", "ktx",
        ],
    ),
    ext(ARCHIVES_NESTED, true, &["zip", "jar", "apk", "epub", "7z"]),
    ext(
        ARCHIVES_NESTED,
        false,
        &["rar", "gz", "tgz", "xz", "zst", "bz2", "tar"],
    ),
];

/// Number of leading bytes [`classify`] may look at (covers every offset in [`SIGNATURES`]).
pub const HEAD_LEN: usize = 0x8001 + 16;

/// Lower-case extension of a file name; none for `.hidden` names and names without a dot.
fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() && !e.is_empty() => e.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Class of a file. `rel_path` uses `/` separators; `head` is the start of the file's content
/// (at most [`HEAD_LEN`] bytes are looked at).
pub fn classify(rel_path: &str, head: &[u8]) -> &'static str {
    let mut parts: Vec<&str> = rel_path.split('/').collect();
    let name = parts.pop().unwrap_or_default();
    if parts.iter().any(|p| p.eq_ignore_ascii_case(".git")) {
        return SOURCE_GIT;
    }
    let ext = extension(name);
    if let Some(s) = SIGNATURES.iter().find(|s| s.matches(head, &ext)) {
        return s.class_for(&ext);
    }
    match EXTENSIONS.iter().find(|r| r.exts.contains(&ext.as_str())) {
        Some(r) if !r.verified => r.class,
        _ => OTHER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\0\x10JFIF";
    const PNG: &[u8] = b"\x89PNG\r\n\x1A\n\0\0\0\rIHDR";

    #[test]
    fn extension_and_magic_agree() {
        assert_eq!(classify("a/b.jpg", JPEG), PHOTO_JPEG);
        assert_eq!(classify("x.PNG", PNG), PHOTO_RAW_PNG);
        assert_eq!(classify("doc.pdf", b"%PDF-1.7\n"), OFFICE_PDF);
        assert_eq!(classify("m.mp4", b"\0\0\0\x18ftypisom"), VIDEO);
        assert_eq!(classify("lib.so", b"\x7FELF\x02"), SOFTWARE_INSTALLED);
        assert_eq!(
            classify("w.safetensors", b"\x10\0\0\0\0\0\0\0{"),
            MODEL_WEIGHTS
        );
        assert_eq!(classify("main.rs", b"fn main() {}"), SOURCE_GIT);
        assert_eq!(classify("a.log", b"2020-01-01 start"), LOGS_TEXT);
        assert_eq!(classify("n.txt", b"hello"), TEXT_PROSE);
    }

    #[test]
    fn magic_wins_over_extension() {
        assert_eq!(classify("fake.jpg", PNG), PHOTO_RAW_PNG);
        assert_eq!(classify("noext", JPEG), PHOTO_JPEG);
        assert_eq!(classify("notes.txt", b"%PDF-1.4"), OFFICE_PDF);
        // A bare "MZ" is only a PE header for executable extensions.
        assert_eq!(classify("a.txt", b"MZ rest"), TEXT_PROSE);
        assert_eq!(classify("a.exe", b"MZ\x90\0"), SOFTWARE_INSTALLED);
    }

    #[test]
    fn container_signatures_refine_by_extension() {
        let zip = b"PK\x03\x04rest";
        assert_eq!(classify("a.docx", zip), OFFICE_PDF);
        assert_eq!(classify("a.xlsx", zip), OFFICE_PDF);
        assert_eq!(classify("a.apk", zip), ARCHIVES_NESTED);
        assert_eq!(classify("a.epub", zip), ARCHIVES_NESTED);
        assert_eq!(classify("model.pth", zip), MODEL_WEIGHTS);
        let ole = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
        assert_eq!(classify("a.doc", ole), OFFICE_PDF);
        assert_eq!(classify("setup.msi", ole), SOFTWARE_INSTALLED);
        assert_eq!(classify("a.m4a", b"\0\0\0\x20ftypM4A "), AUDIO);
        assert_eq!(classify("a.wav", b"RIFF\0\0\0\0WAVEfmt "), AUDIO);
        assert_eq!(classify("a.avi", b"RIFF\0\0\0\0AVI LIST"), VIDEO);
    }

    #[test]
    fn verified_extension_without_magic_is_other() {
        assert_eq!(classify("empty.jpg", b""), OTHER);
        assert_eq!(classify("page.png", b"<html>404</html>"), OTHER);
        assert_eq!(classify("x.docx", b"not a zip"), OTHER);
        // Unverified extensions are trusted.
        assert_eq!(classify("song.mp3", b"\xFF\xFBdata"), AUDIO);
        assert_eq!(classify("big.iso", b"short"), VM_IMAGE);
    }

    #[test]
    fn unknown_is_other() {
        assert_eq!(classify("mystery.xyz", b"data"), OTHER);
        assert_eq!(classify("README", b"text"), OTHER);
        assert_eq!(classify(".hidden", b"text"), OTHER);
        assert_eq!(classify("ends.with.dot.", b""), OTHER);
    }

    #[test]
    fn files_under_git_are_source() {
        assert_eq!(
            classify(".git/objects/ab/cdef", b"\x78\x01data"),
            SOURCE_GIT
        );
        assert_eq!(classify("proj/.git/pack/p.pack", b"PACK"), SOURCE_GIT);
        assert_eq!(classify("proj/.GIT/x.jpg", JPEG), SOURCE_GIT);
        // Only directories count, and only the exact name.
        assert_eq!(classify("proj/.gitignore", b"target"), OTHER);
        assert_eq!(classify("proj/not.git/x.jpg", JPEG), PHOTO_JPEG);
    }

    #[test]
    fn tables_are_consistent() {
        let mut seen = std::collections::HashSet::new();
        for r in EXTENSIONS {
            for e in r.exts {
                assert!(seen.insert(*e), "extension `{e}` listed twice");
                assert_eq!(*e, e.to_ascii_lowercase());
            }
        }
        for s in SIGNATURES {
            for (off, bytes) in s.checks {
                assert!(off + bytes.len() <= HEAD_LEN, "{} exceeds HEAD_LEN", s.name);
            }
        }
        let mut names = std::collections::HashSet::new();
        assert!(SIGNATURES.iter().all(|s| names.insert(s.name)));
    }

    #[test]
    fn iso_signature_sits_at_its_offset() {
        let mut head = vec![0u8; HEAD_LEN];
        head[0x8001..0x8006].copy_from_slice(b"CD001");
        assert_eq!(classify("disc.bin", &head), VM_IMAGE);
        assert_eq!(classify("disc.bin", &head[..0x8001]), OTHER);
    }
}
