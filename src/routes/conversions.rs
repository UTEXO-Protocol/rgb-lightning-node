use super::*;

impl From<RgbLibBalance> for AssetBalanceResponse {
    fn from(value: RgbLibBalance) -> Self {
        Self {
            settled: value.settled,
            future: value.future,
            spendable: value.spendable,
            offchain_outbound: 0,
            offchain_inbound: 0,
        }
    }
}

impl From<RgbLibAssetCFA> for AssetCFA {
    fn from(value: RgbLibAssetCFA) -> Self {
        Self {
            asset_id: value.asset_id,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            media: value.media.map(|m| m.into()),
        }
    }
}

impl From<RgbLibAssetNIA> for AssetNIA {
    fn from(value: RgbLibAssetNIA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            media: value.media.map(|m| m.into()),
        }
    }
}

impl From<AssetSchema> for RgbLibAssetSchema {
    fn from(value: AssetSchema) -> Self {
        match value {
            AssetSchema::Nia => Self::Nia,
            AssetSchema::Uda => Self::Uda,
            AssetSchema::Cfa => Self::Cfa,
        }
    }
}

impl From<RgbLibAssetSchema> for AssetSchema {
    fn from(value: RgbLibAssetSchema) -> Self {
        match value {
            RgbLibAssetSchema::Nia => Self::Nia,
            RgbLibAssetSchema::Uda => Self::Uda,
            RgbLibAssetSchema::Cfa => Self::Cfa,
            RgbLibAssetSchema::Ifa => todo!(),
        }
    }
}

impl From<RgbLibAssetUDA> for AssetUDA {
    fn from(value: RgbLibAssetUDA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            token: value.token.map(|t| t.into()),
        }
    }
}

impl From<RgbLibAssignment> for Assignment {
    fn from(x: RgbLibAssignment) -> Self {
        match x {
            RgbLibAssignment::Fungible(amt) => Self::Fungible(amt),
            RgbLibAssignment::NonFungible => Self::NonFungible,
            RgbLibAssignment::InflationRight(amt) => Self::InflationRight(amt),
            RgbLibAssignment::ReplaceRight => Self::ReplaceRight,
            RgbLibAssignment::Any => Self::Any,
        }
    }
}

impl From<Assignment> for RgbLibAssignment {
    fn from(x: Assignment) -> Self {
        match x {
            Assignment::Fungible(amt) => Self::Fungible(amt),
            Assignment::NonFungible => Self::NonFungible,
            Assignment::InflationRight(amt) => Self::InflationRight(amt),
            Assignment::ReplaceRight => Self::ReplaceRight,
            Assignment::Any => Self::Any,
        }
    }
}

impl From<Network> for BitcoinNetwork {
    fn from(x: Network) -> Self {
        match x {
            Network::Bitcoin => Self::Mainnet,
            Network::Testnet => Self::Testnet,
            Network::Testnet4 => Self::Testnet4,
            Network::Regtest => Self::Regtest,
            Network::Signet => Self::Signet,
            _ => unimplemented!("unsupported network"),
        }
    }
}

impl From<RgbLibNetwork> for BitcoinNetwork {
    fn from(x: RgbLibNetwork) -> Self {
        match x {
            RgbLibNetwork::Mainnet => Self::Mainnet,
            RgbLibNetwork::Testnet => Self::Testnet,
            RgbLibNetwork::Testnet4 => Self::Testnet4,
            RgbLibNetwork::Regtest => Self::Regtest,
            RgbLibNetwork::Signet => Self::Signet,
        }
    }
}

impl From<sdk::ChannelStatus> for ChannelStatus {
    fn from(value: sdk::ChannelStatus) -> Self {
        match value {
            sdk::ChannelStatus::Opening => Self::Opening,
            sdk::ChannelStatus::Opened => Self::Opened,
            sdk::ChannelStatus::Closing => Self::Closing,
        }
    }
}

impl From<RgbLibEmbeddedMedia> for EmbeddedMedia {
    fn from(value: RgbLibEmbeddedMedia) -> Self {
        Self {
            mime: value.mime,
            data: value.data,
        }
    }
}

impl From<sdk::HtlcStatus> for HTLCStatus {
    fn from(value: sdk::HtlcStatus) -> Self {
        match value {
            sdk::HtlcStatus::Pending => Self::Pending,
            sdk::HtlcStatus::Succeeded => Self::Succeeded,
            sdk::HtlcStatus::Failed => Self::Failed,
        }
    }
}

impl From<HTLCStatus> for sdk::HtlcStatus {
    fn from(value: HTLCStatus) -> Self {
        match value {
            HTLCStatus::Pending => Self::Pending,
            HTLCStatus::Succeeded => Self::Succeeded,
            HTLCStatus::Failed => Self::Failed,
        }
    }
}

impl From<RgbLibIndexerProtocol> for IndexerProtocol {
    fn from(x: RgbLibIndexerProtocol) -> Self {
        match x {
            RgbLibIndexerProtocol::Electrum => Self::Electrum,
            RgbLibIndexerProtocol::Esplora => Self::Esplora,
        }
    }
}

impl From<RgbLibMedia> for Media {
    fn from(value: RgbLibMedia) -> Self {
        Self {
            file_path: value.file_path,
            digest: value.digest,
            mime: value.mime,
        }
    }
}

impl From<RgbLibProofOfReserves> for ProofOfReserves {
    fn from(value: RgbLibProofOfReserves) -> Self {
        Self {
            utxo: value.utxo.to_string(),
            proof: value.proof,
        }
    }
}

impl From<Recipient> for RgbLibRecipient {
    fn from(value: Recipient) -> Self {
        Self {
            recipient_id: value.recipient_id,
            witness_data: value.witness_data.map(|w| w.into()),
            assignment: value.assignment.into(),
            transport_endpoints: value.transport_endpoints,
        }
    }
}

impl From<RgbLibRecipientType> for RecipientType {
    fn from(value: RgbLibRecipientType) -> Self {
        match value {
            RgbLibRecipientType::Blind => Self::Blind,
            RgbLibRecipientType::Witness => Self::Witness,
        }
    }
}

impl From<sdk::SwapStatus> for SwapStatus {
    fn from(value: sdk::SwapStatus) -> Self {
        match value {
            sdk::SwapStatus::Waiting => Self::Waiting,
            sdk::SwapStatus::Pending => Self::Pending,
            sdk::SwapStatus::Succeeded => Self::Succeeded,
            sdk::SwapStatus::Expired => Self::Expired,
            sdk::SwapStatus::Failed => Self::Failed,
        }
    }
}

impl From<SwapStatus> for sdk::SwapStatus {
    fn from(value: SwapStatus) -> Self {
        match value {
            SwapStatus::Waiting => Self::Waiting,
            SwapStatus::Pending => Self::Pending,
            SwapStatus::Succeeded => Self::Succeeded,
            SwapStatus::Expired => Self::Expired,
            SwapStatus::Failed => Self::Failed,
        }
    }
}

impl From<RgbLibToken> for Token {
    fn from(value: RgbLibToken) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media.map(|em| em.into()),
            media: value.media.map(|m| m.into()),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves.map(|r| r.into()),
        }
    }
}

impl From<RgbLibTokenLight> for TokenLight {
    fn from(value: RgbLibTokenLight) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media,
            media: value.media.map(|m| m.into()),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves,
        }
    }
}

impl From<sdk::EmbeddedMedia> for EmbeddedMedia {
    fn from(value: sdk::EmbeddedMedia) -> Self {
        Self {
            mime: value.mime,
            data: value.data,
        }
    }
}

impl From<sdk::Media> for Media {
    fn from(value: sdk::Media) -> Self {
        Self {
            file_path: value.file_path,
            digest: value.digest,
            mime: value.mime,
        }
    }
}

impl From<sdk::ProofOfReserves> for ProofOfReserves {
    fn from(value: sdk::ProofOfReserves) -> Self {
        Self {
            utxo: value.utxo,
            proof: value.proof,
        }
    }
}

impl From<sdk::Token> for Token {
    fn from(value: sdk::Token) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media.map(Into::into),
            media: value.media.map(Into::into),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves.map(Into::into),
        }
    }
}

impl From<sdk::TokenLight> for TokenLight {
    fn from(value: sdk::TokenLight) -> Self {
        Self {
            index: value.index,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            embedded_media: value.embedded_media,
            media: value.media.map(Into::into),
            attachments: value
                .attachments
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            reserves: value.reserves,
        }
    }
}

impl From<sdk::AssetBalance> for AssetBalanceResponse {
    fn from(value: sdk::AssetBalance) -> Self {
        Self {
            settled: value.settled,
            future: value.future,
            spendable: value.spendable,
            offchain_outbound: value.offchain_outbound,
            offchain_inbound: value.offchain_inbound,
        }
    }
}

impl From<sdk::AssetNIA> for AssetNIA {
    fn from(value: sdk::AssetNIA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            media: value.media.map(Into::into),
        }
    }
}

impl From<sdk::AssetUDA> for AssetUDA {
    fn from(value: sdk::AssetUDA) -> Self {
        Self {
            asset_id: value.asset_id,
            ticker: value.ticker,
            name: value.name,
            details: value.details,
            precision: value.precision,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            token: value.token.map(Into::into),
        }
    }
}

impl From<sdk::AssetCFA> for AssetCFA {
    fn from(value: sdk::AssetCFA) -> Self {
        Self {
            asset_id: value.asset_id,
            name: value.name,
            details: value.details,
            precision: value.precision,
            issued_supply: value.issued_supply,
            timestamp: value.timestamp,
            added_at: value.added_at,
            balance: value.balance.into(),
            media: value.media.map(Into::into),
        }
    }
}

impl From<sdk::TransactionType> for TransactionType {
    fn from(value: sdk::TransactionType) -> Self {
        match value {
            sdk::TransactionType::RgbSend => Self::RgbSend,
            sdk::TransactionType::Drain => Self::Drain,
            sdk::TransactionType::CreateUtxos => Self::CreateUtxos,
            sdk::TransactionType::User => Self::User,
        }
    }
}

impl From<sdk::TransferKind> for TransferKind {
    fn from(value: sdk::TransferKind) -> Self {
        match value {
            sdk::TransferKind::Issuance => Self::Issuance,
            sdk::TransferKind::ReceiveBlind => Self::ReceiveBlind,
            sdk::TransferKind::ReceiveWitness => Self::ReceiveWitness,
            sdk::TransferKind::Send => Self::Send,
            sdk::TransferKind::Inflation => Self::Inflation,
        }
    }
}

impl From<sdk::TransferStatus> for TransferStatus {
    fn from(value: sdk::TransferStatus) -> Self {
        match value {
            sdk::TransferStatus::WaitingCounterparty => Self::WaitingCounterparty,
            sdk::TransferStatus::WaitingConfirmations => Self::WaitingConfirmations,
            sdk::TransferStatus::Settled => Self::Settled,
            sdk::TransferStatus::Failed => Self::Failed,
        }
    }
}

impl From<sdk::TransportType> for TransportType {
    fn from(value: sdk::TransportType) -> Self {
        match value {
            sdk::TransportType::JsonRpc => Self::JsonRpc,
        }
    }
}

impl From<WitnessData> for RgbLibWitnessData {
    fn from(value: WitnessData) -> Self {
        Self {
            amount_sat: value.amount_sat,
            blinding: value.blinding,
        }
    }
}
