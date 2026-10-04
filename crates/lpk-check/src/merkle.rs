//! The Merkle tree over the chunk hashes (section 5).

const NODE: &str = "LitePack lpk v1 Merkle node";
const EMPTY: &str = "LitePack lpk v1 Merkle empty";

/// The root over the leaves, by the split rule of section 5.
pub fn root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return blake3::derive_key(EMPTY, &[]);
    }
    node_root(leaves)
}

fn node_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.len() == 1 {
        return leaves[0];
    }
    let n = leaves.len();
    let mut k = 1usize;
    while k * 2 < n {
        k *= 2;
    }
    let l = node_root(&leaves[..k]);
    let r = node_root(&leaves[k..]);
    let mut m = [0u8; 64];
    m[..32].copy_from_slice(&l);
    m[32..].copy_from_slice(&r);
    blake3::derive_key(NODE, &m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8; 32]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn spec_vectors() {
        assert_eq!(
            hex(&root(&[])),
            "f986dffb57677490beca54d2e3583730ff1b3e102731e6b019a8361c1a4efa0c"
        );
        assert_eq!(
            hex(&root(&[[0x11; 32], [0x22; 32]])),
            "2c3cddfa1e3c1c08f0088621676bc301fa3b581d7aa895c340239273754da9d3"
        );
    }

    #[test]
    fn five_leaf_shape() {
        let l: Vec<[u8; 32]> = (0..5u8).map(|i| [i; 32]).collect();
        let n = |a: [u8; 32], b: [u8; 32]| {
            let mut m = [0u8; 64];
            m[..32].copy_from_slice(&a);
            m[32..].copy_from_slice(&b);
            blake3::derive_key(NODE, &m)
        };
        let want = n(n(n(l[0], l[1]), n(l[2], l[3])), l[4]);
        assert_eq!(root(&l), want);
        assert_eq!(root(&l[..1]), l[0]);
    }
}
