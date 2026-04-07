// Deterministic bitcoin commitments library.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2019-2024 by
//     Dr Maxim Orlovsky <orlovsky@lnp-bp.org>
//
// Copyright (C) 2019-2024 LNP/BP Standards Association. All rights reserved.
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

use bitcoin::key::{Secp256k1, TapTweak, TweakedPublicKey, UntweakedPublicKey};
use bitcoin::taproot::{LeafVersion, TapNodeHash};
use bitcoin::TapLeafHash;

use super::{TapretFirst, TapretNodePartner, TapretPathProof, TapretProof};
use crate::commit_verify::{mpc, ConvolveCommit, ConvolveCommitProof};
use crate::dbc::tapret::tapscript::TapretCommitment;

/// Errors during tapret commitment embedding into x-only public key.
#[derive(Clone, Eq, PartialEq, Debug, Display, Error, From)]
#[display(doc_comments)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename_all = "camelCase")
)]
pub enum TapretKeyError {
    /// tapret node partner {0} contains alternative commitment
    AlternativeCommitment(TapretNodePartner),

    /// tapret node partner {0} has an invalid order with the commitment node
    /// {1:?}
    IncorrectOrdering(TapretNodePartner, TapLeafHash),
}

impl ConvolveCommitProof<mpc::Commitment, UntweakedPublicKey, TapretFirst> for TapretProof {
    type Suppl = TapretPathProof;

    fn restore_original(&self, _: &TweakedPublicKey) -> UntweakedPublicKey { self.internal_pk }

    fn extract_supplement(&self) -> &Self::Suppl { &self.path_proof }
}

impl ConvolveCommit<mpc::Commitment, TapretProof, TapretFirst> for UntweakedPublicKey {
    type Commitment = TweakedPublicKey;
    type CommitError = TapretKeyError;

    fn convolve_commit(
        &self,
        supplement: &TapretPathProof,
        msg: &mpc::Commitment,
    ) -> Result<(TweakedPublicKey, TapretProof), Self::CommitError> {
        let tapret_commitment = TapretCommitment::with(*msg, supplement.nonce);
        let script_commitment = tapret_commitment.commit();

        let merkle_root: TapNodeHash = if let Some(ref partner) = supplement.partner_node {
            if !partner.check_no_commitment() {
                return Err(TapretKeyError::AlternativeCommitment(partner.clone()));
            }

            let commitment_leaf = script_commitment.tapscript_leaf_hash();
            let commitment_hash: TapNodeHash = TapNodeHash::from(commitment_leaf);

            if !partner.check_ordering(commitment_hash) {
                return Err(TapretKeyError::IncorrectOrdering(partner.clone(), commitment_leaf));
            }

            TapNodeHash::from_node_hashes(commitment_hash, partner.tap_node_hash())
        } else {
            TapNodeHash::from_script(&script_commitment, LeafVersion::TapScript)
        };

        let (output_key, _) = self.tap_tweak(&Secp256k1::new(), Some(merkle_root));

        let proof = TapretProof {
            path_proof: supplement.clone(),
            internal_pk: *self,
        };

        Ok((output_key, proof))
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use bitcoin::taproot::LeafScript;

    use super::*;
    use crate::commit_verify::mpc::Commitment;

    #[test]
    fn key_path() {
        let internal_pk = UntweakedPublicKey::from_str(
            "c5f93479093e2b8f724a79844cc10928dd44e9a390b539843fb83fbf842723f3",
        )
        .unwrap();
        let msg = mpc::Commitment::from([8u8; 32]);
        let path_proof = TapretPathProof::root(0);

        // Do via API
        let (outer_key, proof) = internal_pk.convolve_commit(&path_proof, &msg).unwrap();

        // Do manually
        let tapret_commitment = TapretCommitment::with(msg, path_proof.nonce);
        let script_commitment = tapret_commitment.commit();
        let script_leaf = TapNodeHash::from_script(&script_commitment, LeafVersion::TapScript);
        let (real_key, _) = internal_pk.tap_tweak(&Secp256k1::new(), Some(script_leaf));

        assert_eq!(outer_key, real_key);

        assert_eq!(proof, TapretProof {
            path_proof,
            internal_pk
        });

        ConvolveCommitProof::<Commitment, UntweakedPublicKey, TapretFirst>::verify(
            &proof, &msg, &outer_key,
        )
        .unwrap();
    }

    #[test]
    fn single_script() {
        let internal_pk = UntweakedPublicKey::from_str(
            "c5f93479093e2b8f724a79844cc10928dd44e9a390b539843fb83fbf842723f3",
        )
        .unwrap();
        let msg = mpc::Commitment::from([8u8; 32]);
        let path_proof = TapretPathProof::with(
            TapretNodePartner::RightLeaf(LeafScript {
                version: LeafVersion::TapScript,
                script: default!(),
            }),
            1,
        )
        .unwrap();

        let (outer_key, proof) = internal_pk.convolve_commit(&path_proof, &msg).unwrap();

        assert_eq!(proof, TapretProof {
            path_proof,
            internal_pk
        });

        ConvolveCommitProof::<Commitment, UntweakedPublicKey, TapretFirst>::verify(
            &proof, &msg, &outer_key,
        )
        .unwrap();
    }

    #[test]
    #[should_panic(expected = "IncorrectOrdering")]
    fn invalid_partner_ordering() {
        let internal_pk = UntweakedPublicKey::from_str(
            "c5f93479093e2b8f724a79844cc10928dd44e9a390b539843fb83fbf842723f3",
        )
        .unwrap();
        let msg = mpc::Commitment::from([8u8; 32]);
        let path_proof = TapretPathProof::with(
            TapretNodePartner::RightLeaf(LeafScript {
                version: LeafVersion::TapScript,
                script: default!(),
            }),
            11,
        )
        .unwrap();

        let (outer_key, proof) = internal_pk.convolve_commit(&path_proof, &msg).unwrap();

        assert_eq!(proof, TapretProof {
            path_proof,
            internal_pk
        });

        ConvolveCommitProof::<Commitment, UntweakedPublicKey, TapretFirst>::verify(
            &proof, &msg, &outer_key,
        )
        .unwrap();
    }
}
