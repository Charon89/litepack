//! A number together with the result files it was computed from.
//!
//! Every quantity of the report is a [`Traced`]: the value and the set of source ids (indices into
//! the report's list of sources). Arithmetic unions the sets, and the renderer prints them in
//! brackets after the value, so a number can not be shown without saying where it came from.

use std::collections::BTreeSet;
use std::ops::{Add, Div, Mul, Sub};

/// Index (from 1) into the report's list of source files.
pub type SourceId = usize;

#[derive(Debug, Clone, PartialEq)]
pub struct Traced {
    pub value: f64,
    pub sources: BTreeSet<SourceId>,
}

impl Traced {
    pub fn new(value: f64, source: SourceId) -> Traced {
        Traced {
            value,
            sources: BTreeSet::from([source]),
        }
    }

    pub fn from_u64(value: u64, source: SourceId) -> Traced {
        Traced::new(value as f64, source)
    }

    /// The same sources, another value (for a function of this value alone).
    pub fn map(&self, f: impl Fn(f64) -> f64) -> Traced {
        Traced {
            value: f(self.value),
            sources: self.sources.clone(),
        }
    }

    /// The sum of the items; `None` for no items (an empty sum would have no source).
    pub fn sum<'a>(items: impl IntoIterator<Item = &'a Traced>) -> Option<Traced> {
        let mut it = items.into_iter();
        let first = it.next()?.clone();
        Some(it.fold(first, |acc, t| &acc + t))
    }

    /// `[3]`, `[1,3-5]`: the sources, ranges collapsed.
    pub fn tag(&self) -> String {
        brackets(&self.sources)
    }

    /// The value formatted by `f`, then the bracketed sources.
    pub fn show(&self, f: impl Fn(f64) -> String) -> String {
        format!("{} {}", f(self.value), self.tag())
    }
}

/// Source ids as `[1,3-5]`.
pub fn brackets(ids: &BTreeSet<SourceId>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let v: Vec<SourceId> = ids.iter().copied().collect();
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1] == v[j] + 1 {
            j += 1;
        }
        if j > i + 1 {
            parts.push(format!("{}-{}", v[i], v[j]));
        } else {
            for x in &v[i..=j] {
                parts.push(x.to_string());
            }
        }
        i = j + 1;
    }
    format!("[{}]", parts.join(","))
}

macro_rules! op {
    ($tr:ident, $m:ident, $o:tt) => {
        impl $tr<&Traced> for &Traced {
            type Output = Traced;
            fn $m(self, rhs: &Traced) -> Traced {
                Traced {
                    value: self.value $o rhs.value,
                    sources: self.sources.union(&rhs.sources).copied().collect(),
                }
            }
        }
    };
}
op!(Add, add, +);
op!(Sub, sub, -);
op!(Mul, mul, *);
op!(Div, div, /);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_unions_sources() {
        let a = Traced::new(6.0, 2);
        let b = Traced::new(3.0, 7);
        let c = &(&a / &b) + &a;
        assert_eq!(c.value, 8.0);
        assert_eq!(c.sources, BTreeSet::from([2, 7]));
        assert_eq!(a.map(|x| x * 2.0).sources, a.sources);
        assert!(Traced::sum(std::iter::empty()).is_none());
        assert_eq!(Traced::sum([&a, &b]).map(|t| t.value), Some(9.0));
    }

    #[test]
    fn brackets_collapse_runs() {
        let s = |v: &[usize]| brackets(&v.iter().copied().collect());
        assert_eq!(s(&[3]), "[3]");
        assert_eq!(s(&[1, 2]), "[1,2]");
        assert_eq!(s(&[1, 3, 4, 5, 9]), "[1,3-5,9]");
    }
}
