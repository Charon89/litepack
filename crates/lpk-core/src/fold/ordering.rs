//! File order inside a cluster (E2-8, D-12: files are ordered, chunks are not).
//!
//! Two orders: [`FileOrder::None`] keeps the path order of the clustering (directory locality);
//! [`FileOrder::Extension`] sorts by extension (lower-cased), then file name (lower-cased), then
//! path, the order 7-Zip uses for solid archives. Neither reads a file. (A similarity order that
//! sketched file contents was built and measured on this branch's history and is not part of the
//! tree.) Ordering never changes what deduplicates: that is content-addressed in the writer.

use crate::ingest::Input;

/// How files are ordered inside a cluster.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileOrder {
    /// Path order (what the clustering gives).
    #[default]
    None,
    /// Extension (lower-cased), then name, then path.
    Extension,
}

impl FileOrder {
    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            FileOrder::None => "none",
            FileOrder::Extension => "extension",
        }
    }
}

/// Sort by extension (lower-cased; none sorts first), then file name (lower-cased), then path.
pub fn extension_order(inputs: &mut [Input]) {
    inputs.sort_by_cached_key(|i| {
        let name = i.path.rsplit('/').next().unwrap_or(&i.path).to_string();
        let ext = match name.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
            _ => String::new(),
        };
        (ext, name.to_lowercase(), i.path.clone())
    });
}

/// Order one cluster's files (given in path order).
pub fn order_cluster(inputs: &mut [Input], mode: FileOrder) {
    match mode {
        FileOrder::None => {}
        FileOrder::Extension => extension_order(inputs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_format::{EntryFlags, EntryKind};
    use std::path::PathBuf;

    fn input(path: &str) -> Input {
        Input {
            path: path.into(),
            kind: EntryKind::File,
            len: 1,
            mtime_ns: 0,
            flags: EntryFlags::default(),
            source: PathBuf::from(path),
            symlink_target: None,
            identity: None,
        }
    }

    fn paths(v: &[Input]) -> Vec<&str> {
        v.iter().map(|i| i.path.as_str()).collect()
    }

    #[test]
    fn extension_order_cases_ties_and_no_extension() {
        let mut v = vec![
            input("z/b.TXT"),
            input("a/b.txt"),
            input("m/README"),
            input("a/a.c"),
            input(".hidden"),
            input("b/a.txt"),
        ];
        order_cluster(&mut v, FileOrder::Extension);
        assert_eq!(
            paths(&v),
            [".hidden", "m/README", "a/a.c", "b/a.txt", "a/b.txt", "z/b.TXT"]
        );
        let mut v = vec![input("y/f.txt"), input("x/f.txt")];
        order_cluster(&mut v, FileOrder::Extension);
        assert_eq!(paths(&v), ["x/f.txt", "y/f.txt"]);
    }

    #[test]
    fn none_keeps_the_given_order_and_is_the_default() {
        assert_eq!(FileOrder::default(), FileOrder::None);
        let mut v = vec![input("b.z"), input("a.y")];
        order_cluster(&mut v, FileOrder::None);
        assert_eq!(paths(&v), ["b.z", "a.y"]);
    }
}
