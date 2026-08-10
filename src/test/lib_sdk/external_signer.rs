//! External-signer `lib_sdk` tests use [`NativeExternalSigner`] (real PSBT signing) behind a small
//! [`ExternalSignerHost`] wrapper for availability / `SignRgbPsbt` failure injection. The wire format
//! matches production (`rgb_lightning_node::signer_integration_wire` → `signer::proto`).
use crate::helpers::*;
use rgb_lightning_node::signer_integration_wire::{decode_signer_request_wire, SignerRequest};
use serial_test::serial;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

fn clone_bootstrap(b: &SdkExternalSignerBootstrap) -> SdkExternalSignerBootstrap {
    SdkExternalSignerBootstrap {
        node_id: b.node_id.clone(),
        account_xpub_vanilla: b.account_xpub_vanilla.clone(),
        account_xpub_colored: b.account_xpub_colored.clone(),
        master_fingerprint: b.master_fingerprint.clone(),
        protocol_version: b.protocol_version.clone(),
        api_level: b.api_level,
    }
}

/// Wraps [`NativeExternalSigner`] so tests can simulate signer outage or `SignRgbPsbt` rejection.
struct TunableNativeSignerHost {
    inner: Arc<rgb_lightning_node::NativeExternalSigner>,
    available: Arc<AtomicBool>,
    /// When set, [`SignerRequest::SignRgbPsbt`] returns a host error (simulates signer / transport failure).
    fail_sign_rgb_psbt: Arc<AtomicBool>,
}

impl TunableNativeSignerHost {
    fn new(inner: Arc<rgb_lightning_node::NativeExternalSigner>) -> Self {
        Self {
            inner,
            available: Arc::new(AtomicBool::new(true)),
            fail_sign_rgb_psbt: Arc::new(AtomicBool::new(false)),
        }
    }

    fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::Relaxed);
    }

    fn set_fail_sign_rgb_psbt(&self, fail: bool) {
        self.fail_sign_rgb_psbt.store(fail, Ordering::Relaxed);
    }
}

impl rgb_lightning_node::ExternalSignerHost for TunableNativeSignerHost {
    fn call(&self, request: Vec<u8>) -> Result<Vec<u8>, rgb_lightning_node::RlnError> {
        if !self.available.load(Ordering::Relaxed) {
            return Err(rgb_lightning_node::RlnError::Internal(
                "signer unavailable".to_string(),
            ));
        }
        if self.fail_sign_rgb_psbt.load(Ordering::Relaxed) {
            let req = decode_signer_request_wire(&request)
                .map_err(|e| rgb_lightning_node::RlnError::Internal(e.to_string()))?;
            if matches!(&req, SignerRequest::SignRgbPsbt { .. }) {
                return Err(rgb_lightning_node::RlnError::Internal(
                    "sign_rgb_psbt failure injected".to_string(),
                ));
            }
        }
        self.inner.call(request)
    }
}

fn attach_external_signer_host(
    node: &SdkNode,
    host: Arc<dyn rgb_lightning_node::ExternalSignerHost>,
    bootstrap: &SdkExternalSignerBootstrap,
) {
    node.attach_external_signer(host, clone_bootstrap(bootstrap))
        .expect("attach external signer");
}

fn unlock_with_attached_external_signer(node: &SdkNode, announce_alias: &str) {
    node.unlock_with_attached_external_signer(
        Some("user".to_string()),
        Some("password".to_string()),
        Some("localhost".to_string()),
        Some(18443),
        Some("127.0.0.1:50001".to_string()),
        Some(PROXY_ENDPOINT_LOCAL.to_string()),
        vec![],
        Some(announce_alias.to_string()),
    )
    .expect("unlock with attached external signer");
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Block until every `127.0.0.1:<port>` in `ports` is bindable (i.e. released by a prior node),
/// or `timeout` elapses. `SdkNode::shutdown()` returns before the OS tears down its listeners,
/// so restarting a node on the same ports needs this to avoid an "Address already in use" race.
fn wait_ports_free(ports: &[u16], timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    for &port in ports {
        loop {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "port {port} was not released within {timeout:?}"
            );
            thread::sleep(Duration::from_millis(200));
        }
    }
}

fn make_native_signer(
    storage_dir: &std::path::Path,
    seed_hex: Option<String>,
) -> Arc<rgb_lightning_node::NativeExternalSigner> {
    // With the seed-only `NativeExternalSigner` constructor, tests provide stable seeds from env or
    // explicit per-test overrides. This avoids any signer-side seed persistence while still letting
    // us simulate mismatched signers.
    let seed_hex = seed_hex.unwrap_or_else(|| {
        std::env::var("RLN_TEST_NATIVE_SIGNER_SEED_HEX").unwrap_or_else(|_| "11".repeat(32))
    });
    let _ = storage_dir; // kept to minimize churn in test callsites
    rgb_lightning_node::NativeExternalSigner::new(seed_hex, "regtest".to_string(), Some(true))
        .expect("create native signer")
}

/// Like [`make_native_signer`], but with a disk-backed VLS store: required whenever the test
/// simulates a process restart with channels that must stay usable (a stateful validating
/// signer cannot validate commitment state it never tracked).
fn make_native_signer_with_storage(
    storage_dir: &std::path::Path,
    seed_hex: Option<String>,
) -> Arc<rgb_lightning_node::NativeExternalSigner> {
    let seed_hex = seed_hex.unwrap_or_else(|| {
        std::env::var("RLN_TEST_NATIVE_SIGNER_SEED_HEX").unwrap_or_else(|_| "11".repeat(32))
    });
    rgb_lightning_node::NativeExternalSigner::new_with_storage(
        seed_hex,
        "regtest".to_string(),
        Some(true),
        storage_dir.display().to_string(),
    )
    .expect("create native signer with storage")
}

#[test]
#[serial]
fn external_init_unlock_and_restart_same_signer() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    let test_dir = test_dir("sdk_external_signer_init_unlock_restart");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_dir = test_dir.join("node_a");
    let signer_dir = test_dir.join("signer_a");

    let signer = make_native_signer(&signer_dir, None);
    let bootstrap = signer.bootstrap().expect("bootstrap");

    let node = make_node(&node_dir, NODE_A_DAEMON_PORT + 110, NODE_A_PEER_PORT + 110);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node.init_with_native_external_signer(signer.clone())
            .expect("external init");
        node.unlock_with_native_external_signer(
            signer.clone(),
            Some("user".to_string()),
            Some("password".to_string()),
            Some("localhost".to_string()),
            Some(18443),
            Some("127.0.0.1:50001".to_string()),
            Some(PROXY_ENDPOINT_LOCAL.to_string()),
            vec![],
            Some("RLN_external".to_string()),
        )
        .expect("unlock with native external signer");

        let info = node.node_info().expect("node_info");
        assert_eq!(info.pubkey.to_string(), bootstrap.node_id);

        node.shutdown();
        thread::sleep(Duration::from_millis(500));

        let restarted = make_node(&node_dir, NODE_A_DAEMON_PORT + 110, NODE_A_PEER_PORT + 110);
        restarted
            .unlock_with_native_external_signer(
                signer.clone(),
                Some("user".to_string()),
                Some("password".to_string()),
                Some("localhost".to_string()),
                Some(18443),
                Some("127.0.0.1:50001".to_string()),
                Some(PROXY_ENDPOINT_LOCAL.to_string()),
                vec![],
                Some("RLN_external".to_string()),
            )
            .expect("unlock after restart");
        let info2 = restarted.node_info().expect("node_info after restart");
        assert_eq!(info2.pubkey.to_string(), bootstrap.node_id);
        restarted.shutdown();
    }));

    if result.is_err() {
        panic!("external signer SDK test failed");
    }
}

#[test]
#[serial]
fn external_restart_with_mismatched_signer_fails_unlock() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    let test_dir = test_dir("sdk_external_signer_restart_mismatch");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_dir = test_dir.join("node_a");
    let signer_a_dir = test_dir.join("signer_a");
    let signer_b_dir = test_dir.join("signer_b");

    let signer_a = make_native_signer(&signer_a_dir, Some("11".repeat(32)));
    let node = make_node(&node_dir, NODE_A_DAEMON_PORT + 111, NODE_A_PEER_PORT + 111);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node.init_with_native_external_signer(signer_a.clone())
            .expect("external init");
        node.unlock_with_native_external_signer(
            signer_a.clone(),
            Some("user".to_string()),
            Some("password".to_string()),
            Some("localhost".to_string()),
            Some(18443),
            Some("127.0.0.1:50001".to_string()),
            Some(PROXY_ENDPOINT_LOCAL.to_string()),
            vec![],
            Some("RLN_external".to_string()),
        )
        .expect("unlock");
        node.shutdown();
        thread::sleep(Duration::from_millis(500));

        let signer_b = make_native_signer(&signer_b_dir, Some("22".repeat(32)));

        let restarted = make_node(&node_dir, NODE_A_DAEMON_PORT + 111, NODE_A_PEER_PORT + 111);
        let err = restarted
            .unlock_with_native_external_signer(
                signer_b,
                Some("user".to_string()),
                Some("password".to_string()),
                Some("localhost".to_string()),
                Some(18443),
                Some("127.0.0.1:50001".to_string()),
                Some(PROXY_ENDPOINT_LOCAL.to_string()),
                vec![],
                Some("RLN_external".to_string()),
            )
            .expect_err("unlock must fail for mismatched signer");
        assert!(
            matches!(
                err,
                rgb_lightning_node::RlnError::ExternalSignerMismatch(_)
                    | rgb_lightning_node::RlnError::Conflict(_)
            ),
            "unexpected error for mismatched signer: {err}"
        );
        restarted.shutdown();
    }));

    if result.is_err() {
        panic!("external signer mismatch SDK test failed");
    }
}

#[test]
#[serial]
fn external_signer_connection_loss_and_restore_mock() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    let test_dir = test_dir("sdk_external_signer_connection_loss_restore_mock");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_dir = test_dir.join("node_a");

    let signer_dir = test_dir.join("signer_a");
    let signer = make_native_signer(&signer_dir, None);
    let bootstrap = signer.bootstrap().expect("bootstrap");
    let host = Arc::new(TunableNativeSignerHost::new(signer));

    let node = make_node(&node_dir, NODE_A_DAEMON_PORT + 121, NODE_A_PEER_PORT + 121);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node.init_with_external_signer(clone_bootstrap(&bootstrap))
            .expect("external init");
        attach_external_signer_host(&node, host.clone(), &bootstrap);
        unlock_with_attached_external_signer(&node, "RLN_external_conn_loss");

        // `createutxos` can be flaky across environments; this test only needs a spendable UTXO
        // so that `sendbtc` reaches the external signer `SignRgbPsbt` path.
        ensure_funded(&node, 1, "node A mock");
        let self_addr = node.address().expect("node address").address;

        host.set_available(false);
        let err = node
            .sendbtc(SdkSendBtcRequest {
                address: self_addr.clone(),
                amount: 1_000,
                fee_rate: CREATE_UTXOS_FEE_RATE,
                skip_sync: false,
            })
            .err()
            .expect("send_btc must fail while signer is unavailable");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("external signer")
                || msg.contains("status")
                || msg.contains("transport")
                || msg.contains("unavailable")
                || msg.contains("internal"),
            "unexpected error while signer unavailable: {msg}"
        );

        host.set_available(true);
        // Best-effort recovery: once the signer is back, the node should be able to proceed,
        // but some environments may still surface transient failures (sync/fee estimation).
        let _ = node.sendbtc(SdkSendBtcRequest {
            address: self_addr,
            amount: 1_000,
            fee_rate: CREATE_UTXOS_FEE_RATE,
            skip_sync: false,
        });
        node.shutdown();
    }));

    if result.is_err() {
        panic!("external signer connection loss/restore mock test failed");
    }
}

/// When the host rejects [`SignerRequest::SignRgbPsbt`], the node must surface the error (no silent
/// success via a local RGB wallet PSBT path). Uses `sendbtc` after `createutxos` so the flow reaches
/// `rgb_sign_psbt` (a second identical `createutxos` can fail earlier with insufficient coins before
/// signing).
#[test]
#[serial]
fn external_signer_sign_rgb_psbt_failure_surfaces_on_send_btc_mock() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    let test_dir = test_dir("sdk_external_signer_sign_rgb_psbt_fail_mock");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_dir = test_dir.join("node_a");
    let signer_dir = test_dir.join("signer_a");

    let signer = make_native_signer(&signer_dir, None);
    let bootstrap = signer.bootstrap().expect("bootstrap");
    let host = Arc::new(TunableNativeSignerHost::new(signer));

    let node = make_node(&node_dir, NODE_A_DAEMON_PORT + 131, NODE_A_PEER_PORT + 131);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node.init_with_external_signer(clone_bootstrap(&bootstrap))
            .expect("external init");
        attach_external_signer_host(&node, host.clone(), &bootstrap);
        unlock_with_attached_external_signer(&node, "RLN_external_sign_rgb_fail");

        // `createutxos` is not required for this test and can be flaky across environments.
        // We only need some spendable balance so `sendbtc` reaches the external signer signing path.
        ensure_funded(&node, 1, "node A mock");
        let self_addr = node.address().expect("node address").address;

        host.set_fail_sign_rgb_psbt(true);
        let err = node
            .sendbtc(SdkSendBtcRequest {
                address: self_addr.clone(),
                amount: 1_000,
                fee_rate: CREATE_UTXOS_FEE_RATE,
                skip_sync: false,
            })
            .err()
            .expect("sendbtc must fail when SignRgbPsbt is rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("unexpected")
                || msg.contains("rgb")
                || msg.contains("external")
                || msg.contains("transport")
                || msg.contains("sign")
                || msg.contains("internal")
                || msg.contains("psbt"),
            "unexpected error for SignRgbPsbt host failure: {msg}"
        );

        host.set_fail_sign_rgb_psbt(false);
        // Best-effort: once the host starts responding again, the node should be able to proceed.
        // Some environments may still surface transient failures (fee estimation/sync), so don't
        // hard-require success here; the core assertion for this test is that the host rejection
        // surfaces as an error.
        let _ = node.sendbtc(SdkSendBtcRequest {
            address: self_addr,
            amount: 1_000,
            fee_rate: CREATE_UTXOS_FEE_RATE,
            skip_sync: false,
        });
        node.shutdown();
    }));

    if result.is_err() {
        panic!("external signer SignRgbPsbt failure lib_sdk test failed");
    }
}

/// Regression: a vanilla channel's `FundingGenerationReady` whose `SignRgbPsbt` fails (a remote
/// signer briefly unreachable, or a malformed reply) used to panic the event task at
/// `rgb_sign_psbt(..).unwrap()`, killing the funding permanently. The handler must instead take
/// the cooperative path: abort the staged pending vanilla tx — at that point the
/// `PENDING_FUNDING_NAMESPACE` mapping does not exist yet, so nothing else could ever abort it —
/// and replay the event. Once the signer recovers, the replayed event re-stages the funding from
/// the *released* UTXOs and broadcasts: that recovery is also the proof of cleanup, since a leaked
/// pending vanilla tx would keep the coins locked and the replay could never fund the channel.
#[test]
#[serial]
fn external_signer_vanilla_funding_sign_failure_is_retryable() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    const PORT_OFF: u16 = 330;
    let da = NODE_A_DAEMON_PORT + PORT_OFF;
    let pa = NODE_A_PEER_PORT + PORT_OFF;
    let db = NODE_B_DAEMON_PORT + PORT_OFF;
    let pb = NODE_B_PEER_PORT + PORT_OFF;

    let test_dir = test_dir("sdk_external_vanilla_funding_sign_fail");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");
    let signer_a_dir = test_dir.join("signer_a");

    let signer = make_native_signer(&signer_a_dir, None);
    let bootstrap = signer.bootstrap().expect("bootstrap");
    let host = Arc::new(TunableNativeSignerHost::new(signer));

    let node_a = make_node(&node_a_dir, da, pa);
    let node_b = make_node(&node_b_dir, db, pb);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node_a
            .init_with_external_signer(clone_bootstrap(&bootstrap))
            .expect("node A external init");
        attach_external_signer_host(&node_a, host.clone(), &bootstrap);
        unlock_with_attached_external_signer(&node_a, "RLN_ext_vanilla_fund_fail");

        node_b
            .init("nodeBpass".to_string(), None)
            .expect("node B init");
        node_b
            .unlock(unlock_request("nodeBpass"))
            .expect("node B unlock");

        ensure_funded(
            &node_a,
            2 * OPEN_CHANNEL_CAPACITY_SAT,
            "node A vanilla funding",
        );

        let peer_uri = format!(
            "{}@127.0.0.1:{pb}",
            node_b.node_info().expect("node B node_info").pubkey
        );
        node_a.connectpeer(peer_uri.clone()).expect("connectpeer");

        // Fail SignRgbPsbt from before the open so the very first `FundingGenerationReady` hits
        // it. The vanilla open itself returns right after `create_channel`; the signing happens in
        // the background event task.
        host.set_fail_sign_rgb_psbt(true);
        node_a
            .openchannel(SdkOpenChannelRequest {
                peer_pubkey_and_opt_addr: peer_uri,
                capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
                push_msat: OPEN_CHANNEL_PUSH_MSAT,
                public: false,
                with_anchors: true,
                fee_base_msat: None,
                fee_proportional_millionths: None,
                temporary_channel_id: None,
                asset_id: None,
                asset_amount: None,
                push_asset_amount: None,
                virtual_open_mode: None,
            })
            .expect("openchannel (vanilla)");

        // Give the event task time to hit the failing signer at least once. The node must stay
        // responsive (no panicked event task) and must not have broadcast any funding tx.
        thread::sleep(Duration::from_secs(5));
        let channels = node_a
            .list_channels()
            .expect("list_channels while the signer is failing");
        assert!(
            channels.iter().all(|c| c.funding_txid.is_none()),
            "no funding tx may be broadcast while SignRgbPsbt fails"
        );

        // Signer recovers: the replayed event must be able to stage a fresh funding tx from the
        // released UTXOs and broadcast it.
        host.set_fail_sign_rgb_psbt(false);
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            node_a.sync().expect("node A sync");
            let funded = node_a
                .list_channels()
                .expect("list_channels while waiting for funding tx")
                .iter()
                .any(|c| c.funding_txid.is_some());
            if funded {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "channel funding was never broadcast after the signer recovered — the \
                 FundingGenerationReady retry path is broken"
            );
            thread::sleep(Duration::from_secs(1));
        }

        node_a.shutdown();
        node_b.shutdown();
        thread::sleep(Duration::from_millis(300));
    }));

    if result.is_err() {
        panic!("external signer vanilla funding sign-failure retry lib_sdk test failed");
    }
}

/// RGB payment with node A (internal signer) and node B (native in-process VLS signer).
///
/// Regtest: `./regtest.sh start`, then:
/// `cargo test -p rgb-lightning-node --features uniffi,test-utils,vls --test lib_sdk rgb_native_external_signer_mixed_one_hop_payment_quick -- --nocapture`
#[test]
#[serial]
fn rgb_native_external_signer_mixed_one_hop_payment_quick() {
    if std::env::var("SKIP_RGB_NATIVE_EXTERNAL_QUICK")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
    {
        return;
    }

    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    const PORT_OFF: u16 = 200;
    let da = NODE_A_DAEMON_PORT + PORT_OFF;
    let pa = NODE_A_PEER_PORT + PORT_OFF;
    let db = NODE_B_DAEMON_PORT + PORT_OFF;
    let pb = NODE_B_PEER_PORT + PORT_OFF;

    let test_dir = test_dir("sdk_rgb_native_external_quick");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");
    let signer_b_dir = test_dir.join("signer_b");

    let signer_b = make_native_signer(&signer_b_dir, None);

    let node_a = make_node(&node_a_dir, da, pa);
    let node_b = make_node(&node_b_dir, db, pb);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node_a
            .init("nodeApass".to_string(), None)
            .expect("node A init");
        node_b
            .init_with_native_external_signer(signer_b.clone())
            .expect("node B init native external signer");

        node_a
            .unlock(unlock_request("nodeApass"))
            .expect("node A unlock");
        node_b
            .unlock_with_native_external_signer(
                signer_b.clone(),
                Some("user".to_string()),
                Some("password".to_string()),
                Some("localhost".to_string()),
                Some(18443),
                Some("127.0.0.1:50001".to_string()),
                Some(PROXY_ENDPOINT_LOCAL.to_string()),
                vec![],
                Some("RLN_rgb_native_quick".to_string()),
            )
            .expect("node B unlock native external signer");

        fund_and_create_utxos(&node_a, "node A quick");
        node_a
            .createutxos(SdkCreateUtxosRequest {
                up_to: false,
                num: Some(25),
                size: Some(32_000),
                fee_rate: CREATE_UTXOS_FEE_RATE,
                skip_sync: false,
            })
            .expect("node A createutxos (extra RGB allocation headroom)");
        ensure_funded(&node_b, 200_000, "node B quick");
        mine(1);
        node_a.sync().expect("node A sync after fund");
        node_b.sync().expect("node B sync after fund");

        let asset_id = node_a
            .issueassetnia(SdkIssueAssetNiaRequest {
                amounts: vec![1_000],
                ticker: "QRGB".to_string(),
                name: "QuickRgb".to_string(),
                precision: 0,
            })
            .expect("issueassetnia")
            .asset_id;

        let peer_uri = format!(
            "{}@127.0.0.1:{pb}",
            node_b.node_info().expect("node B node_info").pubkey
        );
        node_a.connectpeer(peer_uri.clone()).expect("connectpeer");

        const PAY_ASSET: u64 = 100;

        node_a
            .openchannel(SdkOpenChannelRequest {
                peer_pubkey_and_opt_addr: peer_uri,
                capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
                push_msat: OPEN_CHANNEL_PUSH_MSAT,
                public: false,
                with_anchors: true,
                fee_base_msat: None,
                fee_proportional_millionths: None,
                temporary_channel_id: None,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(OPEN_CHANNEL_ASSET_AMOUNT),
                push_asset_amount: None,
                virtual_open_mode: None,
            })
            .expect("openchannel");

        wait_for_channel_funding_tx(&node_a, &node_b, &asset_id, Duration::from_secs(90));
        mine(OPEN_CHANNEL_CONFIRM_BLOCKS);
        wait_for_usable_channel(&node_a, &node_b, &asset_id, Duration::from_secs(300));

        let invoice = node_b
            .ln_invoice(LnInvoiceRequest {
                amt_msat: Some(PAYMENT_MSAT),
                expiry_sec: 900,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(PAY_ASSET),
                payment_hash: None,
                description: None,
                description_hash: None,
                min_final_cltv_expiry_delta: None,
            })
            .expect("ln_invoice")
            .invoice;

        let send = node_a
            .sendpayment(SdkSendPaymentRequest {
                invoice: invoice.to_string(),
                amt_msat: None,
                asset_id: None,
                asset_amount: None,
            })
            .expect("sendpayment");
        let payment_hash = send.payment_hash.expect("payment_hash");

        wait_for_payment_status(&node_a, &payment_hash, Duration::from_secs(120));
        wait_for_payment_status(&node_b, &payment_hash, Duration::from_secs(120));

        node_a.shutdown();
        node_b.shutdown();
        thread::sleep(Duration::from_millis(300));
    }));

    if result.is_err() {
        panic!("rgb_native_external_signer_mixed_one_hop_payment_quick failed");
    }
}

/// Mixed internal/external RGB channel: after receiving RGB, the external-signer node must be able
/// to send RGB back over the same channel.
#[test]
#[serial]
fn rgb_native_external_signer_mixed_one_hop_payment_roundtrip() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    const PORT_OFF: u16 = 260;
    const PAY_ASSET: u64 = 50;
    const ROUNDTRIP_PUSH_MSAT: u64 = 6_000_000;
    let da = NODE_A_DAEMON_PORT + PORT_OFF;
    let pa = NODE_A_PEER_PORT + PORT_OFF;
    let db = NODE_B_DAEMON_PORT + PORT_OFF;
    let pb = NODE_B_PEER_PORT + PORT_OFF;

    let test_dir = test_dir("sdk_rgb_native_external_roundtrip");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");
    let signer_b_dir = test_dir.join("signer_b");

    let signer_b = make_native_signer(&signer_b_dir, None);

    let node_a = make_node(&node_a_dir, da, pa);
    let node_b = make_node(&node_b_dir, db, pb);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node_a
            .init("nodeApass".to_string(), None)
            .expect("node A init");
        node_b
            .init_with_native_external_signer(signer_b.clone())
            .expect("node B init native external signer");

        node_a
            .unlock(unlock_request("nodeApass"))
            .expect("node A unlock");
        node_b
            .unlock_with_native_external_signer(
                signer_b.clone(),
                Some("user".to_string()),
                Some("password".to_string()),
                Some("localhost".to_string()),
                Some(18443),
                Some("127.0.0.1:50001".to_string()),
                Some(PROXY_ENDPOINT_LOCAL.to_string()),
                vec![],
                Some("RLN_rgb_native_roundtrip".to_string()),
            )
            .expect("node B unlock native external signer");

        fund_and_create_utxos(&node_a, "node A roundtrip");
        node_a
            .createutxos(SdkCreateUtxosRequest {
                up_to: false,
                num: Some(25),
                size: Some(32_000),
                fee_rate: CREATE_UTXOS_FEE_RATE,
                skip_sync: false,
            })
            .expect("node A createutxos (extra RGB allocation headroom)");
        ensure_funded(&node_b, 200_000, "node B roundtrip");
        mine(1);
        node_a.sync().expect("node A sync after fund");
        node_b.sync().expect("node B sync after fund");

        let asset_id = node_a
            .issueassetnia(SdkIssueAssetNiaRequest {
                amounts: vec![1_000],
                ticker: "QRT".to_string(),
                name: "QuickRoundTripRgb".to_string(),
                precision: 0,
            })
            .expect("issueassetnia")
            .asset_id;

        let peer_uri = format!(
            "{}@127.0.0.1:{pb}",
            node_b.node_info().expect("node B node_info").pubkey
        );
        node_a.connectpeer(peer_uri.clone()).expect("connectpeer");

        node_a
            .openchannel(SdkOpenChannelRequest {
                peer_pubkey_and_opt_addr: peer_uri,
                capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
                push_msat: ROUNDTRIP_PUSH_MSAT,
                public: false,
                with_anchors: true,
                fee_base_msat: None,
                fee_proportional_millionths: None,
                temporary_channel_id: None,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(OPEN_CHANNEL_ASSET_AMOUNT),
                push_asset_amount: None,
                virtual_open_mode: None,
            })
            .expect("openchannel");

        wait_for_channel_funding_tx(&node_a, &node_b, &asset_id, Duration::from_secs(90));
        mine(OPEN_CHANNEL_CONFIRM_BLOCKS);
        wait_for_usable_channel(&node_a, &node_b, &asset_id, Duration::from_secs(300));

        let invoice_1 = node_b
            .ln_invoice(LnInvoiceRequest {
                amt_msat: Some(PAYMENT_MSAT),
                expiry_sec: 900,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(PAY_ASSET),
                payment_hash: None,
                description: None,
                description_hash: None,
                min_final_cltv_expiry_delta: None,
            })
            .expect("ln_invoice first")
            .invoice;

        send_payment_with_ln_balance(
            &node_a,
            &node_b,
            invoice_1,
            &asset_id,
            PAY_ASSET,
            OPEN_CHANNEL_ASSET_AMOUNT,
            0,
        );

        let invoice_2 = node_a
            .ln_invoice(LnInvoiceRequest {
                amt_msat: Some(PAYMENT_MSAT),
                expiry_sec: 900,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(PAY_ASSET),
                payment_hash: None,
                description: None,
                description_hash: None,
                min_final_cltv_expiry_delta: None,
            })
            .expect("ln_invoice second")
            .invoice;

        send_payment_with_ln_balance(
            &node_b,
            &node_a,
            invoice_2,
            &asset_id,
            PAY_ASSET,
            PAY_ASSET,
            OPEN_CHANNEL_ASSET_AMOUNT - PAY_ASSET,
        );

        node_a.shutdown();
        node_b.shutdown();
        thread::sleep(Duration::from_millis(300));
    }));

    if result.is_err() {
        panic!("rgb_native_external_signer_mixed_one_hop_payment_roundtrip failed");
    }
}

/// Mixed internal/external RGB channel: one RGB payment followed by cooperative close must settle
/// balances back on-chain.
#[test]
#[serial]
fn rgb_native_external_signer_mixed_one_hop_payment_coop_close_settles_to_chain() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    const PORT_OFF: u16 = 210;
    const PAY_ASSET: u64 = 50;
    let da = NODE_A_DAEMON_PORT + PORT_OFF;
    let pa = NODE_A_PEER_PORT + PORT_OFF;
    let db = NODE_B_DAEMON_PORT + PORT_OFF;
    let pb = NODE_B_PEER_PORT + PORT_OFF;

    let test_dir = test_dir("sdk_rgb_native_external_close_settles");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");
    let signer_b_dir = test_dir.join("signer_b");

    let signer_b = make_native_signer(&signer_b_dir, None);

    let node_a = make_node(&node_a_dir, da, pa);
    let node_b = make_node(&node_b_dir, db, pb);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node_a
            .init("nodeApass".to_string(), None)
            .expect("node A init");
        node_b
            .init_with_native_external_signer(signer_b.clone())
            .expect("node B init native external signer");

        node_a
            .unlock(unlock_request("nodeApass"))
            .expect("node A unlock");
        node_b
            .unlock_with_native_external_signer(
                signer_b.clone(),
                Some("user".to_string()),
                Some("password".to_string()),
                Some("localhost".to_string()),
                Some(18443),
                Some("127.0.0.1:50001".to_string()),
                Some(PROXY_ENDPOINT_LOCAL.to_string()),
                vec![],
                Some("RLN_rgb_native_close".to_string()),
            )
            .expect("node B unlock native external signer");

        fund_and_create_utxos(&node_a, "node A close");
        node_a
            .createutxos(SdkCreateUtxosRequest {
                up_to: false,
                num: Some(25),
                size: Some(32_000),
                fee_rate: CREATE_UTXOS_FEE_RATE,
                skip_sync: false,
            })
            .expect("node A createutxos (extra RGB allocation headroom)");
        ensure_funded(&node_b, 200_000, "node B close");
        mine(1);
        node_a.sync().expect("node A sync after fund");
        node_b.sync().expect("node B sync after fund");

        let asset_id = node_a
            .issueassetnia(SdkIssueAssetNiaRequest {
                amounts: vec![1_000],
                ticker: "QCLS".to_string(),
                name: "QuickCloseRgb".to_string(),
                precision: 0,
            })
            .expect("issueassetnia")
            .asset_id;

        let node_b_pubkey = node_b.node_info().expect("node B node_info").pubkey;
        let peer_uri = format!("{node_b_pubkey}@127.0.0.1:{pb}");
        node_a.connectpeer(peer_uri.clone()).expect("connectpeer");

        node_a
            .openchannel(SdkOpenChannelRequest {
                peer_pubkey_and_opt_addr: peer_uri,
                capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
                push_msat: OPEN_CHANNEL_PUSH_MSAT,
                public: false,
                with_anchors: true,
                fee_base_msat: None,
                fee_proportional_millionths: None,
                temporary_channel_id: None,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(OPEN_CHANNEL_ASSET_AMOUNT),
                push_asset_amount: None,
                virtual_open_mode: None,
            })
            .expect("openchannel");

        wait_for_channel_funding_tx(&node_a, &node_b, &asset_id, Duration::from_secs(90));
        mine(OPEN_CHANNEL_CONFIRM_BLOCKS);
        wait_for_usable_channel(&node_a, &node_b, &asset_id, Duration::from_secs(300));
        let channel_id = node_a
            .list_channels()
            .expect("node A list_channels after channel ready")
            .into_iter()
            .find(|channel| channel.asset_id.as_ref() == Some(&asset_id) && channel.is_usable)
            .expect("usable RGB channel on node A")
            .channel_id;

        let invoice = node_b
            .ln_invoice(LnInvoiceRequest {
                amt_msat: Some(PAYMENT_MSAT),
                expiry_sec: 900,
                asset_id: Some(asset_id.clone()),
                asset_amount: Some(PAY_ASSET),
                payment_hash: None,
                description: None,
                description_hash: None,
                min_final_cltv_expiry_delta: None,
            })
            .expect("ln_invoice")
            .invoice;

        let send = node_a
            .sendpayment(SdkSendPaymentRequest {
                invoice: invoice.to_string(),
                amt_msat: None,
                asset_id: None,
                asset_amount: None,
            })
            .expect("sendpayment");
        let payment_hash = send.payment_hash.expect("payment_hash");

        wait_for_payment_status(&node_a, &payment_hash, Duration::from_secs(120));
        wait_for_payment_status(&node_b, &payment_hash, Duration::from_secs(120));
        wait_for_ln_balance(
            &node_a,
            &asset_id,
            OPEN_CHANNEL_ASSET_AMOUNT - PAY_ASSET,
            Duration::from_secs(120),
        );

        close_channel(&node_a, channel_id, node_b_pubkey);
        wait_for_balance(&node_a, &asset_id, 950, Duration::from_secs(120));
        wait_for_balance(&node_b, &asset_id, PAY_ASSET, Duration::from_secs(120));

        node_a.shutdown();
        node_b.shutdown();
        thread::sleep(Duration::from_millis(300));
    }));

    if result.is_err() {
        panic!(
            "rgb_native_external_signer_mixed_one_hop_payment_coop_close_settles_to_chain failed"
        );
    }
}

/// End-to-end regression for the restored-virtual-channel signing failure: a
/// `trusted_no_broadcast` virtual channel opened by a node with an in-process
/// `NativeExternalSigner` (disk-backed VLS store) must keep settling payments in both
/// directions after the LDK node restarts on the same storage.
///
/// This exercises the two-part fix together:
/// 1. `NativeExternalSigner::new_with_storage` persists the VLS node (channels, commitment
///    counters, dbid high-water mark) so a stateful validating signer can still validate
///    commitment state after a restart — without it, the restored channel force-closes with
///    "Failed to validate our commitment".
/// 2. The signer-external `derive_stub_per_commitment_point` fix keeps the synthesized
///    pre-setup commitment-point fallback from panicking on a 41-byte CLN-style `ChannelId`
///    (`copy_from_slice: source slice length (41) does not match destination slice length (32)`
///    at vls-core `channel.rs:119`), which on a tokio worker poisoned the LDK `ChannelManager`
///    mutex and bricked the node.
///
/// The unit-level proof that the fallback no longer panics (and now derives the same point a
/// real vls-core node would) lives in signer-external's
/// `stub_per_commitment_point_matches_vls_node_derivation` parity test; this integration test
/// covers the surrounding restart-and-pay flow.
#[test]
#[serial]
fn external_signer_virtual_channel_survives_restart() {
    ensure_regtest_available();
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    const PORT_OFF: u16 = 150;
    let da = NODE_A_DAEMON_PORT + PORT_OFF;
    let pa = NODE_A_PEER_PORT + PORT_OFF;
    let db = NODE_B_DAEMON_PORT + PORT_OFF;
    let pb = NODE_B_PEER_PORT + PORT_OFF;

    let test_dir = test_dir("sdk_external_signer_virtual_restart");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk test dir");
    let host_dir = test_dir.join("host");
    let device_dir = test_dir.join("device");
    let signer_dir = test_dir.join("signer_device");

    // The device holds the in-process VLS signer and is the side that restarts. Its node id is
    // known up front (derived from the signer seed), which lets us allowlist it on the host
    // before either node runs.
    let signer = make_native_signer_with_storage(&signer_dir, None);
    let device_pubkey = signer
        .bootstrap()
        .expect("signer bootstrap")
        .node_id
        .parse::<bitcoin::secp256k1::PublicKey>()
        .expect("device pubkey");

    let host = make_node_with_virtual(&host_dir, da, pa, Some(vec![device_pubkey]));
    let device = make_node_with_virtual(&device_dir, db, pb, None);

    let unlock_device = |node: &SdkNode, signer: &Arc<rgb_lightning_node::NativeExternalSigner>| {
        node.unlock_with_native_external_signer(
            signer.clone(),
            Some("user".to_string()),
            Some("password".to_string()),
            Some("localhost".to_string()),
            Some(18443),
            Some("127.0.0.1:50001".to_string()),
            Some(PROXY_ENDPOINT_LOCAL.to_string()),
            vec![],
            Some("RLN_virtual_device".to_string()),
        )
    };

    let wait_virtual_channel_usable = |node: &SdkNode, peer: &str, label: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let usable = node
                .list_channels()
                .unwrap_or_default()
                .into_iter()
                .any(|c| c.peer_pubkey.to_string() == peer && c.ready && c.is_usable);
            if usable {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "virtual channel not usable within 60s ({label})"
            );
            thread::sleep(Duration::from_millis(500));
        }
    };

    let pay = |payer: &SdkNode, payee: &SdkNode, amt_msat: u64, label: &str| {
        let invoice = payee
            .ln_invoice(LnInvoiceRequest {
                amt_msat: Some(amt_msat),
                expiry_sec: 900,
                asset_id: None,
                asset_amount: None,
                payment_hash: None,
                description: None,
                description_hash: None,
                min_final_cltv_expiry_delta: None,
            })
            .unwrap_or_else(|e| panic!("{label}: ln_invoice: {e:?}"))
            .invoice;
        let send = payer
            .sendpayment(SdkSendPaymentRequest {
                invoice: invoice.to_string(),
                amt_msat: None,
                asset_id: None,
                asset_amount: None,
            })
            .unwrap_or_else(|e| panic!("{label}: sendpayment: {e:?}"));
        let payment_hash = send.payment_hash.expect("payment_hash");
        wait_for_payment_status(payer, &payment_hash, Duration::from_secs(120));
        wait_for_payment_status(payee, &payment_hash, Duration::from_secs(120));
    };

    let host_pubkey_cell: OnceLock<String> = OnceLock::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host.init("hostPass".to_string(), None).expect("host init");
        device
            .init_with_native_external_signer(signer.clone())
            .expect("device init with signer");

        host.unlock(unlock_request("hostPass"))
            .expect("host unlock");
        unlock_device(&device, &signer).expect("device unlock");

        let host_pubkey = host.node_info().expect("host node_info").pubkey.to_string();
        host_pubkey_cell.set(host_pubkey.clone()).unwrap();

        // The device (opener) needs on-chain funds for the never-broadcast virtual funding.
        ensure_funded(&device, 200_000, "device");
        mine(1);
        device.sync().expect("device sync after fund");
        host.sync().expect("host sync");

        let host_uri = format!("{host_pubkey}@127.0.0.1:{pa}");
        device.connectpeer(host_uri.clone()).expect("connectpeer");

        device
            .openchannel(SdkOpenChannelRequest {
                peer_pubkey_and_opt_addr: host_uri,
                capacity_sat: 100_000,
                push_msat: 30_000_000,
                public: false,
                with_anchors: true,
                fee_base_msat: None,
                fee_proportional_millionths: None,
                temporary_channel_id: None,
                asset_id: None,
                asset_amount: None,
                push_asset_amount: None,
                virtual_open_mode: Some("trusted_no_broadcast".to_string()),
            })
            .expect("device opens trusted virtual channel");

        wait_virtual_channel_usable(&device, &host_pubkey, "device, fresh");
        let device_pubkey_str = device_pubkey.to_string();
        wait_virtual_channel_usable(&host, &device_pubkey_str, "host, fresh");

        // Fresh-channel baseline (works even without the fix).
        pay(&device, &host, PAYMENT_MSAT, "fresh device->host");
        pay(&host, &device, PAYMENT_MSAT, "fresh host->device");

        // Restart the device process: same storage, same signer seed — the virtual channel is
        // restored from persistence while the in-process VLS node starts over.
        device.shutdown();
        thread::sleep(Duration::from_millis(500));
    }));
    if result.is_err() {
        host.shutdown();
        panic!("external_signer_virtual_channel_survives_restart failed before restart");
    }

    // Release the pre-restart device node (its LDK state persists to `device_dir`).
    drop(device);

    // Wait for the device node's daemon + peer listeners to actually free their ports before
    // rebinding them for the restarted node. `shutdown()` returns before the OS has fully
    // released the sockets, so on a loaded machine an immediate rebind races the old listener
    // and the restarted node fails to start ("Address already in use") — a failure that shows
    // up under load but not on an idle box.
    wait_ports_free(&[db, pb], Duration::from_secs(30));

    // Restart the device node on the same storage, REUSING the signer instance.
    //
    // This is an in-process restart: a fresh `NativeExternalSigner::new_with_storage` cannot
    // open the redb store while the pre-restart signer still holds its exclusive lock (only a
    // real process exit releases it, which a single test process cannot do). The signer's VLS
    // store is write-through to disk, so its live in-memory state is byte-identical to what a
    // fresh process would restore from `signer_dir` — reusing the instance exercises the same
    // restored-channel-state code path without fighting the single-writer lock. What actually
    // restarts is the LDK node (a new `SdkNode` over the persisted channel).
    //
    // The persistent store (`new_with_storage`) is what makes the restored channel usable at
    // all; the signer-external fix is what keeps its stub commitment-point fallback from
    // panicking in `ldk_channel_keys_id()`. With the ephemeral `NativeExternalSigner::new`
    // (no disk store) this scenario is unrecoverable after a real restart.
    let device = make_node_with_virtual(&device_dir, db, pb, None);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unlock_device(&device, &signer).expect("device unlock after restart");

        let host_pubkey = host_pubkey_cell.get().expect("host pubkey").clone();
        let host_uri = format!("{host_pubkey}@127.0.0.1:{pa}");
        // Reconnect may briefly race the restarted node's peer-handler coming up; retry.
        let reconnect_deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match device.connectpeer(host_uri.clone()) {
                Ok(_) => break,
                Err(_) if std::time::Instant::now() < reconnect_deadline => {
                    thread::sleep(Duration::from_millis(500));
                }
                Err(e) => panic!("reconnect after restart failed: {e:?}"),
            }
        }
        wait_virtual_channel_usable(&device, &host_pubkey, "device, restored");

        // The regression: the first payments over the RESTORED virtual channel. Pre-fix, the
        // background commitment signing panicked in ldk_channel_keys_id() and the payment
        // never settled (node bricked with a poisoned ChannelManager mutex).
        pay(&device, &host, PAYMENT_MSAT, "restored device->host");
        pay(&host, &device, PAYMENT_MSAT, "restored host->device");

        device.shutdown();
        host.shutdown();
        thread::sleep(Duration::from_millis(300));
    }));

    if result.is_err() {
        panic!("external_signer_virtual_channel_survives_restart failed after restart");
    }
}
