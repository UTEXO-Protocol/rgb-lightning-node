//! Test-only distinction between actual Bitcoin funding and trusted virtual channels.

#[derive(Clone, Debug)]
pub(crate) struct ClosedChannelContext {
    pub(crate) funding_txid: Option<String>,
    pub(crate) channel_id: String,
    virtual_open_mode: Option<String>,
}

impl ClosedChannelContext {
    pub(crate) fn new(
        funding_txid: Option<String>,
        channel_id: String,
        virtual_open_mode: Option<String>,
    ) -> Self {
        Self {
            funding_txid,
            channel_id,
            virtual_open_mode,
        }
    }

    pub(crate) fn requires_bitcoin_broadcast(&self) -> bool {
        // A trusted virtual channel deliberately has a synthetic funding outpoint.
        // Unknown modes retain strict Bitcoin admission; RPC errors never grant a bypass.
        self.funding_txid.is_some()
            && self.virtual_open_mode.as_deref() != Some("trusted_no_broadcast")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(mode: Option<&str>, funding: Option<&str>) -> ClosedChannelContext {
        ClosedChannelContext::new(
            funding.map(str::to_owned),
            "exact-channel".into(),
            mode.map(str::to_owned),
        )
    }

    #[test]
    fn real_funding_requires_bitcoin_broadcast() {
        let closed = context(None, Some("real-funding"));
        assert!(closed.requires_bitcoin_broadcast());
        assert_eq!(closed.funding_txid.as_deref(), Some("real-funding"));
        assert_eq!(closed.channel_id, "exact-channel");
    }

    #[test]
    fn trusted_virtual_funding_is_preserved_without_bitcoin_broadcast() {
        let closed = context(Some("trusted_no_broadcast"), Some("synthetic-funding"));
        assert!(!closed.requires_bitcoin_broadcast());
        assert_eq!(closed.funding_txid.as_deref(), Some("synthetic-funding"));
    }

    #[test]
    fn unknown_modes_never_bypass_funded_bitcoin_admission() {
        for mode in [
            "future-mode",
            "",
            "trusted_no_broadcast ",
            "TRUSTED_NO_BROADCAST",
        ] {
            assert!(context(Some(mode), Some("funding")).requires_bitcoin_broadcast());
        }
    }

    #[test]
    fn unfunded_context_preserves_legacy_close_admission() {
        assert!(!context(None, None).requires_bitcoin_broadcast());
        assert!(!context(Some("trusted_no_broadcast"), None).requires_bitcoin_broadcast());
    }
}
