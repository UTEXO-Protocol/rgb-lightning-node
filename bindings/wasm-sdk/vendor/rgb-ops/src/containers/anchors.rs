// RGB ops library for working with smart contracts on Bitcoin & Lightning
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

use std::cmp::Ordering;

use amplify::ByteArray;
use rgb::bitcoin::{Transaction as Tx, Txid};
use rgb::commit_verify::{mpc, CommitEncode, CommitEngine};
use rgb::dbc::{self, Anchor};
use rgb::validation::{DbcProof, EAnchor};
use rgb::{BundleId, DiscloseHash, TransitionBundle};
#[cfg(feature = "serde")]
use serde_crate::{Deserialize, Serialize};
use strict_encoding::StrictDumb;

use crate::{MergeReveal, MergeRevealError, LIB_NAME_RGB_OPS};

#[cfg(feature = "serde")]
mod tx_compat_serde {
    use amplify::hex::{FromHex, ToHex};
    use rgb::bitcoin::{Transaction as Tx, TxIn, TxOut, Witness};
    use serde_crate::{de, Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(crate = "serde_crate")]
    struct BpTxInput {
        #[serde(rename = "prevOutput")]
        prev_output: String,
        #[serde(rename = "sigScript")]
        sig_script: String,
        sequence: u32,
        witness: Vec<String>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(crate = "serde_crate")]
    struct BpTxOutput {
        value: u64,
        #[serde(rename = "scriptPubkey")]
        script_pubkey: String,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(crate = "serde_crate")]
    struct BpTx {
        version: i32,
        inputs: Vec<BpTxInput>,
        outputs: Vec<BpTxOutput>,
        #[serde(rename = "lockTime")]
        lock_time: u32,
    }

    pub fn serialize<S>(tx: &Tx, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        let bp_tx = BpTx {
            version: tx.version.0,
            inputs: tx
                .input
                .iter()
                .map(|input| BpTxInput {
                    prev_output: format!(
                        "{}:{}",
                        input.previous_output.txid, input.previous_output.vout
                    ),
                    sig_script: input.script_sig.to_hex(),
                    sequence: input.sequence.0,
                    witness: input.witness.iter().map(|w| w.to_hex()).collect(),
                })
                .collect(),
            outputs: tx
                .output
                .iter()
                .map(|output| BpTxOutput {
                    value: output.value.to_sat(),
                    script_pubkey: output.script_pubkey.to_hex(),
                })
                .collect(),
            lock_time: tx.lock_time.to_consensus_u32(),
        };
        bp_tx.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Tx, D::Error>
    where D: Deserializer<'de> {
        let bp_tx = BpTx::deserialize(deserializer)?;

        let inputs: Result<Vec<TxIn>, D::Error> = bp_tx
            .inputs
            .into_iter()
            .map(|input| {
                let parts: Vec<&str> = input.prev_output.split(':').collect();
                if parts.len() != 2 {
                    return Err(de::Error::custom("Invalid prevOutput format"));
                }

                let txid = parts[0]
                    .parse()
                    .map_err(|_| de::Error::custom("Invalid txid in prevOutput"))?;
                let vout: u32 = parts[1]
                    .parse()
                    .map_err(|_| de::Error::custom("Invalid vout in prevOutput"))?;

                let script_sig = rgb::bitcoin::ScriptBuf::from_hex(&input.sig_script)
                    .map_err(|_| de::Error::custom("Invalid sigScript hex"))?;

                let witness_data: Result<Vec<Vec<u8>>, D::Error> = input
                    .witness
                    .into_iter()
                    .map(|w| {
                        Vec::<u8>::from_hex(&w)
                            .map_err(|_| de::Error::custom("Invalid witness hex"))
                    })
                    .collect();

                Ok(TxIn {
                    previous_output: rgb::bitcoin::OutPoint { txid, vout },
                    script_sig,
                    sequence: rgb::bitcoin::Sequence(input.sequence),
                    witness: Witness::from_slice(&witness_data?),
                })
            })
            .collect();

        let outputs: Result<Vec<TxOut>, D::Error> = bp_tx
            .outputs
            .into_iter()
            .map(|output| {
                let script_pubkey = rgb::bitcoin::ScriptBuf::from_hex(&output.script_pubkey)
                    .map_err(|_| de::Error::custom("Invalid scriptPubkey hex"))?;

                Ok(TxOut {
                    value: rgb::bitcoin::Amount::from_sat(output.value),
                    script_pubkey,
                })
            })
            .collect();

        Ok(Tx {
            version: rgb::bitcoin::transaction::Version(bp_tx.version),
            lock_time: rgb::bitcoin::absolute::LockTime::from_consensus(bp_tx.lock_time),
            input: inputs?,
            output: outputs?,
        })
    }
}

/// Error merging two [`SealWitness`]es.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Display, Error, From)]
#[display(doc_comments)]
pub enum SealWitnessMergeError {
    /// Error merging two MPC proofs, which are unrelated.
    #[display(inner)]
    #[from]
    MpcMismatch(mpc::MergeError),

    /// Error merging two witness proofs, which are unrelated.
    #[display(inner)]
    #[from]
    WitnessMergeError(MergeRevealError),

    /// seal witnesses can't be merged since they have different DBC proofs.
    DbcMismatch,
}

#[derive(Clone, Eq, PartialEq, Debug)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_RGB_OPS)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename_all = "camelCase")
)]
pub struct SealWitness {
    pub public: PubWitness,
    pub merkle_block: mpc::MerkleBlock,
    pub dbc_proof: DbcProof,
}

impl SealWitness {
    pub fn new(witness: PubWitness, merkle_block: mpc::MerkleBlock, dbc_proof: DbcProof) -> Self {
        SealWitness {
            public: witness,
            merkle_block,
            dbc_proof,
        }
    }

    pub fn witness_id(&self) -> Txid { self.public.to_witness_id() }

    /// Merges two [`SealWitness`]es keeping revealed data.
    pub fn merge_reveal(&mut self, other: &Self) -> Result<(), SealWitnessMergeError> {
        if self.dbc_proof != other.dbc_proof {
            return Err(SealWitnessMergeError::DbcMismatch);
        }
        self.public.merge_reveal(&other.public)?;
        self.merkle_block.merge_reveal(&other.merkle_block)?;
        Ok(())
    }

    pub fn known_bundle_ids(&self) -> impl Iterator<Item = BundleId> {
        let map = self.merkle_block.to_known_message_map().release();
        map.into_values()
            .map(|msg| BundleId::from_byte_array(msg.to_byte_array()))
    }
}

pub trait ToWitnessId {
    fn to_witness_id(&self) -> Txid;
}

impl ToWitnessId for PubWitness {
    fn to_witness_id(&self) -> Txid { self.txid() }
}

impl MergeReveal for PubWitness {
    fn merge_reveal(&mut self, other: &Self) -> Result<(), MergeRevealError> {
        if self == other {
            return Ok(());
        }
        if self.txid() != other.txid() {
            return Err(MergeRevealError::TxidMismatch(self.txid(), other.txid()));
        }
        if let Self::Tx(tx2) = other {
            if let Self::Tx(tx1) = self {
                // Replace each input in tx1 with the one from tx2 if it has more witness or
                // sig_script data
                for (input1, input2) in tx1.input.iter_mut().zip(tx2.input.iter()) {
                    let input1_witness_len: usize = input1.witness.iter().map(|w| w.len()).sum();
                    let input2_witness_len: usize = input2.witness.iter().map(|w| w.len()).sum();
                    match input1_witness_len.cmp(&input2_witness_len) {
                        std::cmp::Ordering::Less => *input1 = input2.clone(),
                        std::cmp::Ordering::Equal => {
                            if input2.script_sig.len() > input1.script_sig.len() {
                                *input1 = input2.clone();
                            }
                        }
                        std::cmp::Ordering::Greater => {}
                    }
                }
            } else {
                *self = other.clone();
            }
        }
        Ok(())
    }
}

#[derive(Clone, Eq, Debug)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_RGB_OPS, tags = custom, dumb = Self::Txid(strict_dumb!()))]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename_all = "camelCase")
)]
pub enum PubWitness {
    #[strict_type(tag = 0x00)]
    Txid(Txid),
    #[strict_type(tag = 0x01)]
    #[cfg_attr(feature = "serde", serde(with = "tx_compat_serde"))]
    Tx(Tx),
}

impl PartialEq for PubWitness {
    fn eq(&self, other: &Self) -> bool { self.txid() == other.txid() }
}

impl Ord for PubWitness {
    fn cmp(&self, other: &Self) -> Ordering { self.txid().cmp(&other.txid()) }
}

impl PartialOrd for PubWitness {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl PubWitness {
    pub fn new(txid: Txid) -> Self { Self::Txid(txid) }

    pub fn with(tx: Tx) -> Self { Self::Tx(tx) }

    pub fn txid(&self) -> Txid {
        match self {
            PubWitness::Txid(txid) => *txid,
            PubWitness::Tx(tx) => tx.compute_txid(),
        }
    }

    pub fn tx(&self) -> Option<&Tx> {
        match self {
            PubWitness::Txid(_) => None,
            PubWitness::Tx(tx) => Some(tx),
        }
    }
}

#[derive(Clone, Eq, Debug)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_RGB_OPS)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename_all = "camelCase")
)]
pub struct WitnessBundle<D: dbc::Proof = DbcProof> {
    pub pub_witness: PubWitness,
    pub anchor: Anchor<D>,
    pub bundle: TransitionBundle,
}

impl<D: dbc::Proof> CommitEncode for WitnessBundle<D> {
    type CommitmentId = DiscloseHash;

    fn commit_encode(&self, e: &mut CommitEngine) { e.commit_to_serialized(&self); }
}

impl<D: dbc::Proof> PartialEq for WitnessBundle<D> {
    fn eq(&self, other: &Self) -> bool { self.pub_witness == other.pub_witness }
}

impl<D: dbc::Proof> Ord for WitnessBundle<D> {
    fn cmp(&self, other: &Self) -> Ordering { self.pub_witness.cmp(&other.pub_witness) }
}

impl<D: dbc::Proof> PartialOrd for WitnessBundle<D> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl<D: dbc::Proof> WitnessBundle<D>
where DbcProof: From<D>
{
    #[inline]
    pub fn with(pub_witness: PubWitness, anchor: Anchor<D>, bundle: TransitionBundle) -> Self {
        Self {
            pub_witness,
            anchor,
            bundle,
        }
    }

    pub fn witness_id(&self) -> Txid { self.pub_witness.to_witness_id() }

    pub fn bundle(&self) -> &TransitionBundle { &self.bundle }

    pub fn bundle_mut(&mut self) -> &mut TransitionBundle { &mut self.bundle }

    pub fn eanchor(&self) -> EAnchor {
        EAnchor::new(self.anchor.mpc_proof.clone(), self.anchor.dbc_proof.clone().into())
    }
}
