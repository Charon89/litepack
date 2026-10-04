//! The Merkle tree over the chunk hashes (spec section 5).

/// Key-derivation context of an internal node.
pub const MERKLE_NODE_CONTEXT: &str = "LitePack lpk v1 Merkle node";
/// Key-derivation context of the root of an empty tree.
pub const MERKLE_EMPTY_CONTEXT: &str = "LitePack lpk v1 Merkle empty";

/// Hash of an internal node over its two children.
fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left);
    buf[32..].copy_from_slice(right);
    blake3::derive_key(MERKLE_NODE_CONTEXT, &buf)
}

fn empty_root() -> [u8; 32] {
    blake3::derive_key(MERKLE_EMPTY_CONTEXT, &[])
}

/// Largest power of two strictly less than `n`; requires `n >= 2`.
fn split_point(n: u64) -> u64 {
    1u64 << (63 - (n - 1).leading_zeros())
}

fn root_of(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.len() == 1 {
        return leaves[0];
    }
    let k = split_point(leaves.len() as u64) as usize;
    node(&root_of(&leaves[..k]), &root_of(&leaves[k..]))
}

/// The Merkle root of `leaves`, computed with recursion depth `log2(n)` and no
/// allocation.
pub fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        empty_root()
    } else {
        root_of(leaves)
    }
}

/// A Merkle tree that keeps every level, so inclusion proofs are cheap.
#[derive(Debug, Clone)]
pub struct MerkleTree {
    /// `levels[0]` are the leaves; the last level holds the root alone.
    levels: Vec<Vec<[u8; 32]>>,
}

impl MerkleTree {
    /// Build the tree over `leaves` in the given order.
    pub fn build(leaves: impl IntoIterator<Item = [u8; 32]>) -> MerkleTree {
        let mut levels = vec![leaves.into_iter().collect::<Vec<_>>()];
        // Pairing bottom-up and promoting an unpaired last node gives the
        // same tree as the recursive split at the largest power of two.
        while levels.last().is_some_and(|l| l.len() > 1) {
            let prev = &levels[levels.len() - 1];
            let next: Vec<[u8; 32]> = prev
                .chunks(2)
                .map(|p| {
                    if p.len() == 2 {
                        node(&p[0], &p[1])
                    } else {
                        p[0]
                    }
                })
                .collect();
            levels.push(next);
        }
        MerkleTree { levels }
    }

    /// Number of leaves.
    pub fn len(&self) -> u64 {
        self.levels[0].len() as u64
    }

    /// True when the tree has no leaves.
    pub fn is_empty(&self) -> bool {
        self.levels[0].is_empty()
    }

    /// The root; the empty-tree root when there are no leaves.
    pub fn root(&self) -> [u8; 32] {
        match self.levels.last().and_then(|l| l.first()) {
            Some(r) => *r,
            None => empty_root(),
        }
    }

    /// The audit path of leaf `index`, bottom sibling first; `None` when the
    /// index is out of range.
    pub fn proof(&self, index: u64) -> Option<Vec<[u8; 32]>> {
        if index >= self.len() {
            return None;
        }
        let mut idx = usize::try_from(index).ok()?;
        let mut path = Vec::new();
        for level in &self.levels[..self.levels.len() - 1] {
            if let Some(s) = level.get(idx ^ 1) {
                path.push(*s);
            }
            idx /= 2;
        }
        Some(path)
    }
}

fn root_from_path(index: u64, len: u64, leaf: &[u8; 32], proof: &[[u8; 32]]) -> Option<[u8; 32]> {
    if len == 1 {
        return proof.is_empty().then_some(*leaf);
    }
    let (top, rest) = proof.split_last()?;
    let k = split_point(len);
    if index < k {
        Some(node(&root_from_path(index, k, leaf, rest)?, top))
    } else {
        Some(node(top, &root_from_path(index - k, len - k, leaf, rest)?))
    }
}

/// Recompute the root from leaf `index` of `len` leaves and its audit path, and
/// compare it with `root`.
pub fn verify_proof(
    root: &[u8; 32],
    index: u64,
    len: u64,
    leaf: &[u8; 32],
    proof: &[[u8; 32]],
) -> bool {
    if index >= len {
        return false;
    }
    root_from_path(index, len, leaf, proof).is_some_and(|r| &r == root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: usize) -> Vec<[u8; 32]> {
        (0..n)
            .map(|i| *blake3::hash(&(i as u64).to_le_bytes()).as_bytes())
            .collect()
    }

    fn hex(b: &[u8; 32]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn two_ways_agree() {
        for n in [0, 1, 2, 3, 4, 5, 8, 1000] {
            let l = leaves(n);
            let t = MerkleTree::build(l.iter().copied());
            assert_eq!(t.len(), n as u64);
            assert_eq!(t.is_empty(), n == 0);
            assert_eq!(t.root(), merkle_root(&l), "n = {n}");
        }
        for n in 1..70 {
            let l = leaves(n);
            assert_eq!(MerkleTree::build(l.iter().copied()).root(), merkle_root(&l));
        }
    }

    #[test]
    fn known_answers() {
        assert_eq!(
            hex(&merkle_root(&[])),
            "f986dffb57677490beca54d2e3583730ff1b3e102731e6b019a8361c1a4efa0c"
        );
        let one = [[0x11u8; 32]];
        assert_eq!(merkle_root(&one), one[0]);
        let two = [[0x11u8; 32], [0x22u8; 32]];
        assert_eq!(
            hex(&merkle_root(&two)),
            "2c3cddfa1e3c1c08f0088621676bc301fa3b581d7aa895c340239273754da9d3"
        );
        assert_eq!(
            hex(&merkle_root(&[[0u8; 32]; 3])),
            "99919ec13ca00f7aa49240d5857a31e4904b212a62bbc1b8cacdf63f0cc35e57"
        );
    }

    #[test]
    fn node_matches_definition() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        let mut cat = a.to_vec();
        cat.extend_from_slice(&b);
        assert_eq!(
            merkle_root(&[a, b]),
            blake3::derive_key("LitePack lpk v1 Merkle node", &cat)
        );
        assert_eq!(
            merkle_root(&[]),
            blake3::derive_key("LitePack lpk v1 Merkle empty", &[])
        );
        // Three leaves split as (2, 1): k is 2, the largest power of two below 3.
        let l = [a, b, [3u8; 32]];
        assert_eq!(merkle_root(&l), node(&node(&a, &b), &[3u8; 32]));
    }

    #[test]
    fn proofs_verify_for_every_leaf() {
        for n in [1usize, 2, 3, 5, 13, 16, 17] {
            let l = leaves(n);
            let t = MerkleTree::build(l.iter().copied());
            let root = t.root();
            for (i, leaf) in l.iter().enumerate() {
                let p = t.proof(i as u64).unwrap();
                assert!(p.len() <= 64 - (n as u64 - 1).leading_zeros() as usize);
                assert!(verify_proof(&root, i as u64, n as u64, leaf, &p), "{n}/{i}");
            }
            assert!(t.proof(n as u64).is_none());
        }
    }

    #[test]
    fn bad_proofs_fail() {
        let n = 13usize;
        let l = leaves(n);
        let t = MerkleTree::build(l.iter().copied());
        let root = t.root();
        for (i, leaf) in l.iter().enumerate() {
            let p = t.proof(i as u64).unwrap();
            for j in 0..p.len() {
                let mut q = p.clone();
                q[j][0] ^= 1;
                assert!(!verify_proof(&root, i as u64, n as u64, leaf, &q));
            }
            let mut longer = p.clone();
            longer.push([0; 32]);
            assert!(!verify_proof(&root, i as u64, n as u64, leaf, &longer));
            if !p.is_empty() {
                assert!(!verify_proof(&root, i as u64, n as u64, leaf, &p[1..]));
            }
            let other = (i + 1) % n;
            assert!(!verify_proof(&root, other as u64, n as u64, leaf, &p));
            assert!(!verify_proof(&root, i as u64, i as u64, leaf, &p));
            let mut bad_leaf = *leaf;
            bad_leaf[5] ^= 0x80;
            assert!(!verify_proof(&root, i as u64, n as u64, &bad_leaf, &p));
        }
        // The proof of the last leaf pins the leaf count. (A proof of a leaf in
        // a left subtree does not: it holds the right subtree only as a hash,
        // so the count is taken from the chunk table, not from the proof.)
        let last = t.proof(n as u64 - 1).unwrap();
        for wrong in (1..=70u64).filter(|w| *w != n as u64) {
            assert!(!verify_proof(&root, n as u64 - 1, wrong, &l[n - 1], &last));
        }
        assert!(!verify_proof(&root, 13, 13, &l[0], &[]));
        assert!(!verify_proof(&root, 0, 0, &l[0], &[]));
    }

    #[test]
    fn million_leaves() {
        let l: Vec<[u8; 32]> = (0..1_000_000u32)
            .map(|i| {
                let mut h = [0u8; 32];
                h[..4].copy_from_slice(&i.to_le_bytes());
                h
            })
            .collect();
        let r = merkle_root(&l);
        assert_ne!(r, merkle_root(&l[..999_999]));
    }
}
