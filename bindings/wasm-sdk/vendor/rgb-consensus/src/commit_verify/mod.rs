// SPDX-License-Identifier: Apache-2.0
//
// Copyright (C) 2025 RGB-Tools developers
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The module provides commitment implementation for RGB.

mod commit;
mod conceal;
mod convolve;
mod digest;
mod embed;
mod id;
pub mod merkle;
pub mod mpc;

pub use commit::{CommitVerify, TryCommitVerify, VerifyError};
pub use conceal::Conceal;
pub use convolve::{ConvolveCommit, ConvolveCommitProof, ConvolveVerifyError};
pub use digest::{Digest, DigestExt, Ripemd160, Sha256};
pub use embed::{EmbedCommitProof, EmbedCommitVerify, EmbedVerifyError, VerifyEq};
pub use id::{
    CommitColType, CommitEncode, CommitEngine, CommitId, CommitLayout, CommitStep, CommitmentId,
    CommitmentLayout, StrictHash,
};
pub use merkle::{MerkleBuoy, MerkleHash, MerkleLeaves, MerkleNode, NodeBranching};

pub const LIB_NAME_COMMIT_VERIFY: &str = "CommitVerify";

/// Marker trait for specific commitment protocols.
///
/// Generic parameter `Protocol` used in commitment scheme traits provides a
/// context & configuration for the concrete implementations.
///
/// Introduction of such generic allows to:
/// - implement trait for foreign data types;
/// - add multiple implementations under different commitment protocols to the combination of the
///   same message and container type (each of which will have its own `Proof` type defined as an
///   associated generic).
pub trait CommitmentProtocol {}

/// Protocol defining commits created by using externally created hash value
/// *optionally pre-tagged*.
pub struct UntaggedProtocol;
impl CommitmentProtocol for UntaggedProtocol {}

/// Helpers for writing test functions working with commit schemes
#[cfg(test)]
pub mod test_helpers {
    use amplify::confinement::SmallVec;
    use amplify::hex::FromHex;

    pub use super::commit::test_helpers::*;
    pub use super::embed::test_helpers::*;

    /// Generates a set of messages for testing purposes
    ///
    /// All of these messages MUST produce different commitments, otherwise the
    /// commitment algorithm is not collision-resistant
    pub fn gen_messages() -> Vec<SmallVec<u8>> {
        vec![
            // empty message
            b"".to_vec(),
            // zero byte message
            b"\x00".to_vec(),
            // text message
            b"test".to_vec(),
            // text length-extended message
            b"test*".to_vec(),
            // short binary message
            Vec::from_hex("deadbeef").unwrap(),
            // length-extended version
            Vec::from_hex("deadbeef00").unwrap(),
            // prefixed version
            Vec::from_hex("00deadbeef").unwrap(),
            // serialized public key as text
            b"0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".to_vec(),
            // the same public key binary data
            Vec::from_hex("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798")
                .unwrap(),
            // different public key
            Vec::from_hex("02f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9")
                .unwrap(),
        ]
        .into_iter()
        .map(|v| SmallVec::try_from(v).unwrap())
        .collect()
    }
}
