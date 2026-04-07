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

use std::collections::HashMap;

use rgb::bitcoin::{Transaction as Tx, Txid};
use rgbcore::validation::{ResolveWitness, WitnessResolverError, WitnessStatus};
use rgbcore::vm::WitnessOrd;
use rgbcore::ChainNet;

use crate::containers::Consignment;

/// Generic struct wrapping any implementation of the [`ResolveWitness`] trait.
/// It also contains a map of the [`Consignment`] TXs, non-empty if `add_consignment_txes` has been
/// called.
#[derive(From)]
#[non_exhaustive]
pub struct AnyResolver {
    inner: Box<dyn ResolveWitness + Send>,
    consignment_txes: HashMap<Txid, Tx>,
}

impl AnyResolver {
    /// Return an [`AnyResolver`] wrapping an [`super::electrum_blocking::ElectrumClient`].
    #[cfg(feature = "electrum_blocking")]
    pub fn electrum_blocking(
        url: &str,
        config: Option<electrum_client::Config>,
    ) -> Result<Self, String> {
        Ok(AnyResolver {
            inner: Box::new(super::electrum_blocking::ElectrumClient {
                inner: electrum_client::Client::from_config(url, config.unwrap_or_default())
                    .map_err(|e| e.to_string())?,
            }),
            consignment_txes: Default::default(),
        })
    }

    /// Return an [`AnyResolver`] wrapping an [`super::esplora_blocking::EsploraClient`].
    #[cfg(feature = "esplora_blocking")]
    pub fn esplora_blocking(builder: esplora_client::Builder) -> Result<Self, String> {
        Ok(AnyResolver {
            inner: Box::new(super::esplora_blocking::EsploraClient {
                inner: esplora_client::BlockingClient::from_builder(builder),
            }),
            consignment_txes: Default::default(),
        })
    }

    /// Return an [`AnyResolver`] wrapping a [`super::mempool_blocking::MemPoolClient`].
    #[cfg(feature = "mempool_blocking")]
    pub fn mempool_blocking(
        _url: &str,
        builder: Option<esplora_client::Builder>,
    ) -> Result<Self, String> {
        Ok(AnyResolver {
            inner: Box::new(super::mempool_blocking::MemPoolClient::new(builder.unwrap())),
            consignment_txes: Default::default(),
        })
    }

    /// Add to the resolver the TXs found in the consignment bundles. Those TXs
    /// will not be resolved by an indexer and will be considered tentative.
    /// Use with caution, this could allow accepting a consignment containing TXs that have not
    /// been broadcasted.
    pub fn add_consignment_txes<const TYPE: bool>(&mut self, consignment: &Consignment<TYPE>) {
        self.consignment_txes.extend(
            consignment
                .bundles
                .iter()
                .filter_map(|bw| bw.pub_witness.tx().cloned())
                .map(|tx| (tx.compute_txid(), tx)),
        );
    }
}

impl ResolveWitness for AnyResolver {
    fn resolve_witness(&self, witness_id: Txid) -> Result<WitnessStatus, WitnessResolverError> {
        if let Some(tx) = self.consignment_txes.get(&witness_id) {
            Ok(WitnessStatus::Resolved(tx.clone(), WitnessOrd::Tentative))
        } else {
            self.inner.resolve_witness(witness_id)
        }
    }

    fn check_chain_net(&self, chain_net: ChainNet) -> Result<(), WitnessResolverError> {
        self.inner.check_chain_net(chain_net)
    }
}
