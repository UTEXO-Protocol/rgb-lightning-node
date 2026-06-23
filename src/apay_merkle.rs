#![allow(dead_code)]

use bitcoin::hashes::{sha256, Hash};

pub(crate) const APAY_HASH_LEAF_TAG: &[u8] = b"UTEXO_APAY_HASH_V1";
pub(crate) const MERKLE_LEAF_PREFIX: u8 = 0x00;
pub(crate) const MERKLE_NODE_PREFIX: u8 = 0x01;

pub(crate) type Hash32 = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MerkleSide {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MerkleProofElement {
    pub(crate) sibling: Hash32,
    pub(crate) side: MerkleSide,
}

fn digest(data: &[u8]) -> Hash32 {
    sha256::Hash::hash(data).to_byte_array()
}

pub(crate) fn leaf_hash(
    recipient_pubkey: &[u8; 33],
    batch_id: &[u8; 16],
    hash_index: u64,
    payment_hash: &[u8; 32],
) -> Hash32 {
    let mut data = Vec::with_capacity(1 + APAY_HASH_LEAF_TAG.len() + 33 + 16 + 8 + 32);
    data.push(MERKLE_LEAF_PREFIX);
    data.extend_from_slice(APAY_HASH_LEAF_TAG);
    data.extend_from_slice(recipient_pubkey);
    data.extend_from_slice(batch_id);
    data.extend_from_slice(&hash_index.to_be_bytes());
    data.extend_from_slice(payment_hash);
    digest(&data)
}

pub(crate) fn node_hash(left: &Hash32, right: &Hash32) -> Hash32 {
    let mut data = Vec::with_capacity(1 + 32 + 32);
    data.push(MERKLE_NODE_PREFIX);
    data.extend_from_slice(left);
    data.extend_from_slice(right);
    digest(&data)
}

fn next_level(level: &[Hash32]) -> Vec<Hash32> {
    let mut next = Vec::with_capacity(level.len().div_ceil(2));
    let mut i = 0;
    while i < level.len() {
        let left = &level[i];
        let right = if i + 1 < level.len() {
            &level[i + 1]
        } else {
            &level[i]
        };
        next.push(node_hash(left, right));
        i += 2;
    }
    next
}

pub(crate) fn root(leaves: &[Hash32]) -> Hash32 {
    assert!(!leaves.is_empty(), "merkle root over empty leaf set");
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        level = next_level(&level);
    }
    level[0]
}

pub(crate) fn proof(leaves: &[Hash32], index: usize) -> Vec<MerkleProofElement> {
    assert!(index < leaves.len(), "merkle proof index out of range");
    let mut out = Vec::new();
    let mut level = leaves.to_vec();
    let mut idx = index;
    while level.len() > 1 {
        let element = if idx.is_multiple_of(2) {
            let sibling = if idx + 1 < level.len() {
                level[idx + 1]
            } else {
                level[idx]
            };
            MerkleProofElement {
                sibling,
                side: MerkleSide::Right,
            }
        } else {
            MerkleProofElement {
                sibling: level[idx - 1],
                side: MerkleSide::Left,
            }
        };
        out.push(element);
        level = next_level(&level);
        idx /= 2;
    }
    out
}

pub(crate) fn verify(leaf: &Hash32, proof: &[MerkleProofElement], expected_root: &Hash32) -> bool {
    let mut current = *leaf;
    for element in proof {
        current = match element.side {
            MerkleSide::Left => node_hash(&element.sibling, &current),
            MerkleSide::Right => node_hash(&current, &element.sibling),
        };
    }
    &current == expected_root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h32(s: &str) -> Hash32 {
        let bytes: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    }

    fn fixture() -> Vec<Hash32> {
        let mut recipient_pubkey = [0x11u8; 33];
        recipient_pubkey[0] = 0x02;
        let mut batch_id = [0u8; 16];
        for (i, b) in batch_id.iter_mut().enumerate() {
            *b = i as u8;
        }
        (1u64..=5)
            .map(|i| leaf_hash(&recipient_pubkey, &batch_id, i, &[i as u8; 32]))
            .collect()
    }

    #[test]
    fn root_matches_reference_vector() {
        let leaves = fixture();
        assert_eq!(leaves.len(), 5);
        assert_eq!(
            leaves[0],
            h32("eabd11aa1b47ea65fa1c5a7cd7992dcaf9f0725ced14eabfebd20a697e8c08a9")
        );
        assert_eq!(
            leaves[4],
            h32("2a583c0c4ee4b0d8d985970116b8035830556eac01a123c0151f8d8dd9fa83fe")
        );
        assert_eq!(
            root(&leaves),
            h32("2a89ca7e910bae70bc3f03f0252ec22de83cc40a5b4ba1582b649be5ea667132")
        );
    }

    #[test]
    fn proof_roundtrips_and_matches_reference() {
        let leaves = fixture();
        let r = root(&leaves);

        let p0 = proof(&leaves, 0);
        assert_eq!(p0.len(), 3);
        assert!(verify(&leaves[0], &p0, &r));
        assert_eq!(
            p0[0].sibling,
            h32("7659a406e2a812a5ed733169fdccf6c02cef92e078c93c11a5332aa654061847")
        );
        assert_eq!(p0[0].side, MerkleSide::Right);

        let p4 = proof(&leaves, 4);
        assert!(verify(&leaves[4], &p4, &r));
        assert_eq!(p4[0].sibling, leaves[4]);
        assert_eq!(p4[0].side, MerkleSide::Right);
        assert_eq!(p4[2].side, MerkleSide::Left);

        let mut bad = leaves[0];
        bad[0] ^= 0x01;
        assert!(!verify(&bad, &p0, &r));
    }

    #[test]
    fn single_leaf_root_is_leaf() {
        let leaves = vec![fixture()[0]];
        assert_eq!(root(&leaves), leaves[0]);
        assert!(proof(&leaves, 0).is_empty());
    }
}
