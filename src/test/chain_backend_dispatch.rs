use std::sync::Arc;

use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};

use crate::ldk_chain_backend::{DynBroadcaster, DynFeeEstimator};

fn _assert_fee_estimator<T: FeeEstimator + ?Sized>(_: &T) {}
fn _assert_broadcaster<T: BroadcasterInterface + ?Sized>(_: &T) {}

// the LDK type aliases are built on trait objects, so every backend must be usable through them
#[test]
fn dyn_chain_backend_implements_required_traits() {
    fn _accepts(fee_estimator: Arc<DynFeeEstimator>, broadcaster: Arc<DynBroadcaster>) {
        _assert_fee_estimator(&*fee_estimator);
        _assert_broadcaster(&*broadcaster);
        let _ = fee_estimator.get_est_sat_per_1000_weight(ConfirmationTarget::AnchorChannelFee);
    }
}

#[cfg(feature = "block-sync")]
#[test]
fn block_sync_backend_coerces_to_dyn() {
    fn _accepts(client: Arc<crate::ldk_chain_backend::block_sync::BitcoindClient>) {
        let _: Arc<DynFeeEstimator> = client.clone();
        let _: Arc<DynBroadcaster> = client;
    }
}

#[cfg(feature = "transaction-sync")]
#[test]
fn transaction_sync_backend_coerces_to_dyn() {
    fn _accepts(client: Arc<crate::ldk_chain_backend::transaction_sync::IndexerClient>) {
        let _: Arc<DynFeeEstimator> = client.clone();
        let _: Arc<DynBroadcaster> = client;
    }
}
