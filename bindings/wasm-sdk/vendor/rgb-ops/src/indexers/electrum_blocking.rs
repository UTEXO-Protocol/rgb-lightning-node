// RGB ops library for working with smart contracts on Bitcoin & Lightning
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2024 by
//     Zoe Faltibà <zoefaltiba@gmail.com>
// Rewritten in 2024 by
//     Dr Maxim Orlovsky <orlovsky@lnp-bp.org>
//
// Copyright (C) 2024 LNP/BP Standards Association. All rights reserved.
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

use std::iter;
use std::num::NonZeroU32;

use amplify::hex::FromHex;
pub use electrum_client;
use electrum_client::{Client, ElectrumApi, Param};
use rgb::bitcoin::constants::ChainHash;
use rgb::bitcoin::{consensus, Transaction as Tx, Txid};
use rgbcore::validation::{ResolveWitness, WitnessResolverError, WitnessStatus};
use rgbcore::vm::{WitnessOrd, WitnessPos};
use rgbcore::ChainNet;

/// Wrapper of an electrum client, necessary to implement the foreign `ResolveWitness` trait.
pub struct ElectrumClient {
    pub inner: Client,
}

impl ResolveWitness for ElectrumClient {
    fn check_chain_net(&self, chain_net: ChainNet) -> Result<(), WitnessResolverError> {
        // check the electrum server is for the correct network
        let block_hash = self
            .inner
            .block_header(0)
            .map_err(|e| WitnessResolverError::ResolverIssue(None, e.to_string()))?
            .block_hash();
        let chain_hash = ChainHash::from_genesis_block_hash(block_hash);
        if chain_net.chain_hash() != chain_hash {
            return Err(WitnessResolverError::WrongChainNet);
        }
        // check the electrum server has the required functionality (verbose
        // transactions)
        let txid = match chain_net {
            ChainNet::BitcoinMainnet => {
                Some("33e794d097969002ee05d336686fc03c9e15a597c1b9827669460fac98799036")
            }
            ChainNet::BitcoinTestnet3 => {
                Some("5e6560fd518aadbed67ee4a55bdc09f19e619544f5511e9343ebba66d2f62653")
            }
            ChainNet::BitcoinTestnet4 => {
                Some("7aa0a7ae1e223414cb807e40cd57e667b718e42aaf9306db9102fe28912b7b4e")
            }
            ChainNet::BitcoinSignet => {
                Some("8153034f45e695453250a8fb7225a5e545144071d8ed7b0d3211efa1f3c92ad8")
            }
            ChainNet::BitcoinSignetCustom(_) => None,
            ChainNet::BitcoinRegtest => {
                Some("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
            }
            _ => return Err(WitnessResolverError::WrongChainNet),
        };
        let txid = if let Some(txid) = txid {
            txid.to_string()
        } else {
            self.inner
                .raw_call("blockchain.transaction.id_from_pos", vec![
                    Param::Usize(1),
                    Param::Usize(0),
                    Param::Bool(false),
                ])
                .map_err(|e| WitnessResolverError::ResolverIssue(None, e.to_string()))?
                .get("tx_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or(WitnessResolverError::InvalidResolverData)?
        };
        if let Err(e) = self
            .inner
            .raw_call("blockchain.transaction.get", vec![Param::String(txid), Param::Bool(true)])
        {
            if !e
                .to_string()
                .contains("genesis block coinbase is not considered an ordinary transaction")
            {
                return Err(WitnessResolverError::ResolverIssue(
                    None,
                    s!("verbose transactions are unsupported by the provided electrum service"),
                ));
            }
        }
        Ok(())
    }

    fn resolve_witness(&self, txid: Txid) -> Result<WitnessStatus, WitnessResolverError> {
        // We get the height of the tip of blockchain
        let header = self
            .inner
            .block_headers_subscribe()
            .map_err(|e| WitnessResolverError::ResolverIssue(Some(txid), e.to_string()))?;

        // Now we get and parse transaction information to get the number of
        // confirmations
        let tx_details = match self.inner.raw_call("blockchain.transaction.get", vec![
            Param::String(txid.to_string()),
            Param::Bool(true),
        ]) {
            Err(e)
                if e.to_string()
                    .contains("No such mempool or blockchain transaction") =>
            {
                return Ok(WitnessStatus::Unresolved);
            }
            Err(e) => return Err(WitnessResolverError::ResolverIssue(Some(txid), e.to_string())),
            Ok(v) => v,
        };
        let forward =
            iter::from_fn(|| self.inner.block_headers_pop().ok().flatten()).count() as isize;

        let Some(tx_hex) = tx_details
            .get("hex")
            .and_then(|v| v.as_str())
            .and_then(|s| Vec::<u8>::from_hex(s).ok())
        else {
            return Err(WitnessResolverError::InvalidResolverData);
        };
        let tx: Tx = consensus::deserialize(&tx_hex)
            .map_err(|_| WitnessResolverError::InvalidResolverData)?;

        let Some(confirmations) = tx_details.get("confirmations") else {
            return Ok(WitnessStatus::Resolved(tx, WitnessOrd::Tentative));
        };
        let confirmations = confirmations
            .as_u64()
            .and_then(|x| u32::try_from(x).ok())
            .ok_or(WitnessResolverError::InvalidResolverData)?;
        if confirmations == 0 {
            return Ok(WitnessStatus::Resolved(tx, WitnessOrd::Tentative));
        }
        let block_time = tx_details
            .get("blocktime")
            .and_then(|v| v.as_i64())
            .ok_or(WitnessResolverError::InvalidResolverData)?;

        let tip_height =
            u32::try_from(header.height).map_err(|_| WitnessResolverError::InvalidResolverData)?;
        let height: isize = (tip_height - confirmations) as isize;
        const SAFETY_MARGIN: isize = 1;
        // first check from expected min to max height
        let get_merkle_res = (1..=forward + 1)
            // we need this under assumption that electrum was lying due to "DB desynchronization"
            // since this have a very low probability we do that after everything else
            .chain((1..=SAFETY_MARGIN).flat_map(|i| [i + forward + 1, 1 - i]))
            .find_map(|offset| self.inner.transaction_get_merkle(&txid, (height + offset) as usize).ok())
            .ok_or_else(|| WitnessResolverError::ResolverIssue(Some(txid), s!("transaction can't be located in the blockchain")))?;

        let tx_height = u32::try_from(get_merkle_res.block_height)
            .map_err(|_| WitnessResolverError::InvalidResolverData)?;

        let height = NonZeroU32::new(tx_height).ok_or(WitnessResolverError::InvalidResolverData)?;
        let pos = WitnessPos::bitcoin(height, block_time)
            .ok_or(WitnessResolverError::InvalidResolverData)?;

        Ok(WitnessStatus::Resolved(tx, WitnessOrd::Mined(pos)))
    }
}
