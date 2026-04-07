// RGB ops library for working with smart contracts on Bitcoin & Lightning
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2019-2023 by
//     Dr Maxim Orlovsky <orlovsky@lnp-bp.org>
//
// Copyright (C) 2019-2023 LNP/BP Standards Association. All rights reserved.
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

use std::num::NonZeroU32;

pub use esplora_client;
use esplora_client::BlockingClient;
use rgb::bitcoin::constants::ChainHash;
use rgb::bitcoin::Txid;
use rgbcore::validation::{ResolveWitness, WitnessResolverError, WitnessStatus};
use rgbcore::vm::{WitnessOrd, WitnessPos};
use rgbcore::ChainNet;

/// Wrapper of an esplora client, necessary to implement the foreign `ResolveWitness` trait.
pub struct EsploraClient {
    pub inner: BlockingClient,
}

impl ResolveWitness for EsploraClient {
    fn check_chain_net(&self, chain_net: ChainNet) -> Result<(), WitnessResolverError> {
        // check the esplora server is for the correct network
        let block_hash = self
            .inner
            .get_block_hash(0)
            .map_err(|e| WitnessResolverError::ResolverIssue(None, e.to_string()))?;
        let chain_hash = ChainHash::from_genesis_block_hash(block_hash);
        if chain_net.chain_hash() != chain_hash {
            return Err(WitnessResolverError::WrongChainNet);
        }
        Ok(())
    }

    fn resolve_witness(&self, txid: Txid) -> Result<WitnessStatus, WitnessResolverError> {
        let Some(tx) = self
            .inner
            .get_tx(&txid)
            .map_err(|e| WitnessResolverError::ResolverIssue(Some(txid), e.to_string()))?
        else {
            return Ok(WitnessStatus::Unresolved);
        };
        let status = self
            .inner
            .get_tx_status(&txid)
            .map_err(|e| WitnessResolverError::ResolverIssue(Some(txid), e.to_string()))?;
        let ord = match status
            .block_height
            .and_then(|h| status.block_time.map(|t| (h, t)))
        {
            Some((h, t)) => {
                let height = NonZeroU32::new(h).ok_or(WitnessResolverError::InvalidResolverData)?;
                WitnessOrd::Mined(
                    WitnessPos::bitcoin(height, t as i64)
                        .ok_or(WitnessResolverError::InvalidResolverData)?,
                )
            }
            None => WitnessOrd::Tentative,
        };
        Ok(WitnessStatus::Resolved(tx, ord))
    }
}
