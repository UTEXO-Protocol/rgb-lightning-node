// RGB wallet library for smart contracts on Bitcoin & Lightning network
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

use std::ops::Deref;
use std::str::FromStr;

use indexmap::IndexMap;
use rgb::bitcoin::constants::ChainHash;
use rgb::bitcoin::hashes::{hash160, sha256};
use rgb::bitcoin::key::{TweakedPublicKey, UntweakedPublicKey};
use rgb::bitcoin::{KnownHrp, Network, PubkeyHash, ScriptHash, WPubkeyHash, WScriptHash};
use rgb::{ChainNet, ContractId, Layer1, SchemaId, SecretSeal, StateType};
use strict_types::FieldName;

use crate::parse::AddressPayload;
use crate::{Amount, NonFungible};

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
#[non_exhaustive]
pub enum RgbTransport {
    JsonRpc { tls: bool, host: String },
    RestHttp { tls: bool, host: String },
    WebSockets { tls: bool, host: String },
    Storm {/* todo */},
    UnspecifiedMeans,
}

#[derive(Clone, PartialEq, Eq, Debug, Display, Error, From)]
#[display(inner)]
pub enum InvoiceStateError {
    #[display(doc_comments)]
    /// could not parse as amount, data, or attach: {0}.
    ParseError(String),
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Display)]
pub enum InvoiceState {
    #[display("")]
    Void,
    #[display("{0}")]
    Amount(Amount),
    #[display(inner)]
    Data(NonFungible),
}

impl FromStr for InvoiceState {
    type Err = InvoiceStateError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            Ok(InvoiceState::Void)
        } else if let Ok(amount) = Amount::from_str(s) {
            Ok(InvoiceState::Amount(amount))
        } else if let Ok(data) = NonFungible::from_str(s) {
            Ok(InvoiceState::Data(data))
        } else {
            Err(InvoiceStateError::ParseError(s.to_owned()))
        }
    }
}

impl From<InvoiceState> for StateType {
    fn from(val: InvoiceState) -> Self {
        match val {
            InvoiceState::Void => StateType::Void,
            InvoiceState::Amount(_) => StateType::Fungible,
            InvoiceState::Data(_) => StateType::Structured,
        }
    }
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
#[non_exhaustive]
pub enum XChainNet<T> {
    BitcoinMainnet(T),
    BitcoinTestnet3(T),
    BitcoinTestnet4(T),
    BitcoinSignet(T),
    BitcoinSignetCustom { chain_hash: ChainHash, data: T },
    BitcoinRegtest(T),
    LiquidMainnet(T),
    LiquidTestnet(T),
}

impl<T> XChainNet<T> {
    pub fn with(cn: ChainNet, data: T) -> Self {
        match cn {
            ChainNet::BitcoinMainnet => XChainNet::BitcoinMainnet(data),
            ChainNet::BitcoinTestnet3 => XChainNet::BitcoinTestnet3(data),
            ChainNet::BitcoinTestnet4 => XChainNet::BitcoinTestnet4(data),
            ChainNet::BitcoinSignet => XChainNet::BitcoinSignet(data),
            ChainNet::BitcoinSignetCustom(chain_hash) => {
                XChainNet::BitcoinSignetCustom { chain_hash, data }
            }
            ChainNet::BitcoinRegtest => XChainNet::BitcoinRegtest(data),
            ChainNet::LiquidMainnet => XChainNet::LiquidMainnet(data),
            ChainNet::LiquidTestnet => XChainNet::LiquidTestnet(data),
        }
    }

    pub fn bitcoin(network: Network, data: T) -> Self {
        match network {
            Network::Bitcoin => Self::BitcoinMainnet(data),
            Network::Testnet => Self::BitcoinTestnet3(data),
            Network::Testnet4 => Self::BitcoinTestnet4(data),
            Network::Signet => Self::BitcoinSignet(data),
            Network::Regtest => Self::BitcoinRegtest(data),
        }
    }

    pub fn chain_network(&self) -> ChainNet {
        match self {
            XChainNet::BitcoinMainnet(_) => ChainNet::BitcoinMainnet,
            XChainNet::BitcoinTestnet3(_) => ChainNet::BitcoinTestnet3,
            XChainNet::BitcoinTestnet4(_) => ChainNet::BitcoinTestnet4,
            XChainNet::BitcoinSignet(_) => ChainNet::BitcoinSignet,
            XChainNet::BitcoinSignetCustom { chain_hash, .. } => {
                ChainNet::BitcoinSignetCustom(*chain_hash)
            }
            XChainNet::BitcoinRegtest(_) => ChainNet::BitcoinRegtest,
            XChainNet::LiquidMainnet(_) => ChainNet::LiquidMainnet,
            XChainNet::LiquidTestnet(_) => ChainNet::LiquidTestnet,
        }
    }

    pub fn into_inner(self) -> T {
        match self {
            XChainNet::BitcoinMainnet(inner)
            | XChainNet::BitcoinTestnet3(inner)
            | XChainNet::BitcoinTestnet4(inner)
            | XChainNet::BitcoinSignet(inner)
            | XChainNet::BitcoinSignetCustom { data: inner, .. }
            | XChainNet::BitcoinRegtest(inner)
            | XChainNet::LiquidMainnet(inner)
            | XChainNet::LiquidTestnet(inner) => inner,
        }
    }

    pub fn layer1(&self) -> Layer1 { self.chain_network().layer1() }

    pub fn address_network(&self) -> KnownHrp {
        match self.chain_network() {
            ChainNet::BitcoinMainnet => KnownHrp::Mainnet,
            ChainNet::BitcoinTestnet3
            | ChainNet::BitcoinTestnet4
            | ChainNet::BitcoinSignet
            | ChainNet::BitcoinSignetCustom(_) => KnownHrp::Testnets,
            ChainNet::BitcoinRegtest => KnownHrp::Regtest,
            ChainNet::LiquidMainnet => KnownHrp::Mainnet,
            ChainNet::LiquidTestnet => KnownHrp::Testnets,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Display, Error)]
#[display(doc_comments)]
pub enum Pay2VoutError {
    /// unexpected address type byte {0:#04x}.
    InvalidAddressType(u8),
    /// invalid taproot output key
    InvalidTapkey,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, From)]
pub struct Pay2Vout(AddressPayload);

impl Pay2Vout {
    pub fn new(address_payload: AddressPayload) -> Self { Pay2Vout(address_payload) }
}

impl Deref for Pay2Vout {
    type Target = AddressPayload;

    fn deref(&self) -> &'_ Self::Target { &self.0 }
}

impl Pay2Vout {
    pub(crate) const P2PKH: u8 = 1;
    pub(crate) const P2SH: u8 = 2;
    pub(crate) const P2WPKH: u8 = 3;
    pub(crate) const P2WSH: u8 = 4;
    pub(crate) const P2TR: u8 = 5;
}

impl TryFrom<[u8; 33]> for Pay2Vout {
    type Error = Pay2VoutError;

    fn try_from(data: [u8; 33]) -> Result<Self, Self::Error> {
        let addr_bytes: [u8; 20] = data[1..21].try_into().unwrap();
        let address = match data[0] {
            Self::P2PKH => AddressPayload::Pkh(PubkeyHash::from_raw_hash(
                *hash160::Hash::from_bytes_ref(&addr_bytes),
            )),
            Self::P2SH => AddressPayload::Sh(ScriptHash::from_raw_hash(
                *hash160::Hash::from_bytes_ref(&addr_bytes),
            )),
            Self::P2WPKH => AddressPayload::Wpkh(WPubkeyHash::from_raw_hash(
                *hash160::Hash::from_bytes_ref(&addr_bytes),
            )),
            Self::P2WSH => AddressPayload::Wsh(WScriptHash::from_raw_hash(
                *sha256::Hash::from_bytes_ref(&data[1..].try_into().unwrap()),
            )),
            Self::P2TR => AddressPayload::Tr(TweakedPublicKey::dangerous_assume_tweaked(
                UntweakedPublicKey::from_slice(&data[1..33])
                    .map_err(|_| Pay2VoutError::InvalidTapkey)?,
            )),
            wrong => return Err(Pay2VoutError::InvalidAddressType(wrong)),
        };
        Ok(Pay2Vout(address))
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, From)]
pub enum Beneficiary {
    #[from]
    BlindedSeal(SecretSeal),
    WitnessVout(Pay2Vout, Option<UntweakedPublicKey>),
}

#[derive(Clone, Eq, PartialEq, Debug)]
#[non_exhaustive]
pub struct RgbInvoice {
    pub transports: Vec<RgbTransport>,
    pub contract: Option<ContractId>,
    pub schema: Option<SchemaId>,
    pub assignment_name: Option<FieldName>,
    pub assignment_state: Option<InvoiceState>,
    pub beneficiary: XChainNet<Beneficiary>,
    /// UTC unix timestamp
    pub expiry: Option<i64>,
    pub unknown_query: IndexMap<String, String>,
}

impl RgbInvoice {
    pub fn chain_network(&self) -> ChainNet { self.beneficiary.chain_network() }
    pub fn address_network(&self) -> KnownHrp { self.beneficiary.address_network() }
    pub fn layer1(&self) -> Layer1 { self.beneficiary.layer1() }
}
