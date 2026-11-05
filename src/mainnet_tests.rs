//! Network restriction tests require neither an unlocked wallet nor external services.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use bitcoin::{
    hashes::{sha256, Hash},
    secp256k1::{Secp256k1, SecretKey},
    ScriptBuf, WPubkeyHash,
};
use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
use rgb_lib::BitcoinNetwork;
use serde_json::{json, Value};
use tokio::{sync::Mutex as TokioMutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    auth::conditional_auth_middleware,
    core_types::{
        asset_link::AssetLinkRequest,
        async_order::{AsyncOrderNewRequest, AsyncOrderOutboundInvoiceRequest},
    },
    disk::FilesystemLogger,
    error::{APIError, APIErrorResponse},
    routes, sdk,
    utils::{AppState, StaticState},
};

const MESSAGE: &str =
    "RLN on mainnet currently supports only on-chain methods. Lightning APIs are not supported.";
const ERROR_NAME: &str = "LightningUnsupportedOnMainnet";
const PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const ASSET: &str = "rgb:Ar4ouaLv-b7f7Dc_-z5EMvtu-FA5KNh1-nlae~jk-8xMBo7E";
const NON_MAINNET_NETWORKS: [BitcoinNetwork; 5] = [
    BitcoinNetwork::Testnet,
    BitcoinNetwork::Testnet4,
    BitcoinNetwork::Signet,
    BitcoinNetwork::SignetCustom,
    BitcoinNetwork::Regtest,
];

struct Fixture {
    state: Arc<AppState>,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new(
        network: BitcoinNetwork,
        root_public_key: Option<biscuit_auth::PublicKey>,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        // No tested call should access wallet state or require a database schema.
        let database = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        let state = Arc::new(AppState {
            static_state: Arc::new(StaticState {
                config: Default::default(),
                ldk_peer_listening_port: 9735,
                network,
                storage_dir_path: path.clone(),
                ldk_data_dir: path.join(".ldk"),
                logger: Arc::new(FilesystemLogger::new(path)),
                max_media_upload_size_mb: 1,
                max_aggregated_media_size_per_channel_mb:
                    crate::rgb_file_transfer::MAX_MEDIA_MB_PER_CHANNEL,
                max_pending_consignments: crate::rgb_file_transfer::MAX_PENDING_CONSIGNMENTS,
                max_media_files_per_channel: crate::rgb_file_transfer::MAX_MEDIA_FILES_PER_CHANNEL,
                enable_virtual_channels_v0: false,
                virtual_peer_pubkeys: vec![],
                database: RwLock::new(Arc::new(database)),
                lsp_base_url: None,
                lsp_bearer_token: None,
                vss_url: None,
                vss_allow_empty_restore: false,
                reuse_addresses: false,
                remote_signer_listen_addr: None,
            }),
            cancel_token: CancellationToken::new(),
            unlocked_app_state: Arc::new(TokioMutex::new(None)),
            ldk_background_services: Arc::new(Mutex::new(None)),
            attached_external_signer: Arc::new(Mutex::new(None)),
            changing_state: Mutex::new(false),
            root_public_key,
            revoked_tokens: Arc::new(Mutex::new(HashSet::new())),
        });
        Self {
            state,
            _directory: directory,
        }
    }

    async fn assert_untouched(&self) {
        assert!(self.state.unlocked_app_state.lock().await.is_none());
        assert!(self.state.ldk_background_services.lock().unwrap().is_none());
        assert!(!*self.state.changing_state.lock().unwrap());
        assert!(!self.state.cancel_token.is_cancelled());
        assert!(!self.state.static_state.ldk_data_dir.exists());
    }
}

struct HttpServer {
    url: String,
    task: JoinHandle<()>,
    client: reqwest::Client,
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpServer {
    async fn new(state: Arc<AppState>) -> Self {
        // Exercise production handlers, JSON/query extraction, error serialization and auth.
        let router = Router::new()
            .route("/apay/new", post(routes::async_order_new))
            .route(
                "/apay/outboundinvoice",
                post(routes::async_order_outbound_invoice),
            )
            .route("/cancelhodlinvoice", post(routes::cancel_hodl_invoice))
            .route("/claimhodlinvoice", post(routes::claim_hodl_invoice))
            .route("/closechannel", post(routes::close_channel))
            .route("/connectpeer", post(routes::connect_peer))
            .route("/decodelninvoice", post(routes::decode_ln_invoice))
            .route("/decodeswapstring", post(routes::decode_swapstring))
            .route("/disconnectpeer", post(routes::disconnect_peer))
            .route("/getchannelid", post(routes::get_channel_id))
            .route("/getpayment", post(routes::get_payment))
            .route("/getswap", post(routes::get_swap))
            .route("/invoicestatus", post(routes::invoice_status))
            .route("/keysend", post(routes::keysend))
            .route("/listchannels", get(routes::list_channels))
            .route("/listpayments", get(routes::list_payments))
            .route("/listpeers", get(routes::list_peers))
            .route("/listswaps", get(routes::list_swaps))
            .route("/lninvoice", post(routes::ln_invoice))
            .route("/makerexecute", post(routes::maker_execute))
            .route("/makerinit", post(routes::maker_init))
            .route("/openchannel", post(routes::open_channel))
            .route("/sendonionmessage", post(routes::send_onion_message))
            .route("/sendpayment", post(routes::send_payment))
            .route("/taker", post(routes::taker))
            .route("/address", post(routes::address))
            .route("/assetlink", post(routes::asset_link))
            .route("/decodergbinvoice", post(routes::decode_rgb_invoice))
            .route("/rgbinvoice", post(routes::rgb_invoice))
            .route("/sendbtc", post(routes::send_btc))
            .route("/sendrgb", post(routes::send_rgb))
            .route("/nodeinfo", get(routes::node_info))
            .route("/networkinfo", get(routes::network_info))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                conditional_auth_middleware,
            ))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self { url, task, client }
    }

    async fn request(&self, path: &str, body: Option<&Value>) -> reqwest::Response {
        let url = format!("{}{path}", self.url);
        let request = match body {
            Some(body) => self.client.post(url).json(body),
            None => self.client.get(url),
        };
        request.send().await.unwrap()
    }
}

fn ln_invoice() -> String {
    let key = SecretKey::from_slice(&[1; 32]).unwrap();
    InvoiceBuilder::new(Currency::Bitcoin)
        .description("network gate test".into())
        .payment_hash(sha256::Hash::hash(&[1; 32]))
        .payment_secret(PaymentSecret([2; 32]))
        .duration_since_epoch(Duration::from_secs(1_700_000_000))
        .min_final_cltv_expiry_delta(144)
        .build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &key))
        .unwrap()
        .to_string()
}

fn swapstring() -> String {
    format!("1/btc/2/{ASSET}/2000000000/{}", "01".repeat(32))
}

fn async_order_request() -> Value {
    json!({"client_node_id": PUBKEY, "params": {
        "hash_index": "0", "payment_hash": "01".repeat(32), "amount_msat": 3000000,
        "description_hash": "02".repeat(32), "invoice_expiry_sec": 900, "min_final_cltv_expiry_delta": 144
    }})
}

fn lightning_requests() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("/apay/new", Some(json!({"host_node_id": PUBKEY}))),
        ("/apay/outboundinvoice", Some(async_order_request())),
        (
            "/cancelhodlinvoice",
            Some(json!({"payment_hash": "01".repeat(32)})),
        ),
        (
            "/claimhodlinvoice",
            Some(json!({"payment_hash": "01".repeat(32), "payment_preimage": "02".repeat(32)})),
        ),
        (
            "/closechannel",
            Some(json!({"channel_id": "01".repeat(32), "peer_pubkey": PUBKEY, "force": false})),
        ),
        (
            "/connectpeer",
            Some(json!({"peer_pubkey_and_addr": format!("{PUBKEY}@127.0.0.1:9735")})),
        ),
        ("/decodelninvoice", Some(json!({"invoice": ln_invoice()}))),
        (
            "/decodeswapstring",
            Some(json!({"swapstring": swapstring()})),
        ),
        ("/disconnectpeer", Some(json!({"peer_pubkey": PUBKEY}))),
        (
            "/getchannelid",
            Some(json!({"temporary_channel_id": "01".repeat(32)})),
        ),
        (
            "/getpayment",
            Some(json!({"payment_hash": "01".repeat(32), "payment_type": "Outbound"})),
        ),
        (
            "/getswap",
            Some(json!({"payment_hash": "01".repeat(32), "taker": false})),
        ),
        ("/invoicestatus", Some(json!({"invoice": ln_invoice()}))),
        (
            "/keysend",
            Some(json!({"dest_pubkey": PUBKEY, "amt_msat": 3000000})),
        ),
        ("/listchannels", None),
        ("/listpayments", None),
        ("/listpeers", None),
        ("/listswaps", None),
        ("/lninvoice", Some(json!({"expiry_sec": 900}))),
        (
            "/makerexecute",
            Some(
                json!({"swapstring": swapstring(), "payment_secret": "01".repeat(32), "taker_pubkey": PUBKEY}),
            ),
        ),
        (
            "/makerinit",
            Some(json!({"qty_from": 1, "qty_to": 2, "to_asset": ASSET, "timeout_sec": 900})),
        ),
        (
            "/openchannel",
            Some(
                json!({"peer_pubkey_and_opt_addr": PUBKEY, "capacity_sat": 100000, "push_msat": 0, "public": false, "with_anchors": true}),
            ),
        ),
        (
            "/sendonionmessage",
            Some(json!({"node_ids": [PUBKEY], "tlv_type": 64, "data": "00"})),
        ),
        ("/sendpayment", Some(json!({"invoice": ln_invoice()}))),
        ("/taker", Some(json!({"swapstring": swapstring()}))),
    ]
}

async fn assert_http_error(response: reqwest::Response, name: &str, path: &str) {
    let status = response.status();
    let error: APIErrorResponse = response.json().await.unwrap();
    assert_eq!(error.name, name, "{path}");
    assert_eq!(error.code, status.as_u16(), "{path}");
    if name == ERROR_NAME {
        assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{path}");
        assert_eq!(error.error, MESSAGE, "{path}");
    }
}

#[tokio::test]
async fn mainnet_rejects_every_lightning_http_handler() {
    let fixture = Fixture::new(BitcoinNetwork::Mainnet, None).await;
    let server = HttpServer::new(fixture.state.clone()).await;
    for (path, body) in lightning_requests() {
        assert_http_error(server.request(path, body.as_ref()).await, ERROR_NAME, path).await;
    }
    fixture.assert_untouched().await;
}

#[tokio::test]
async fn non_mainnet_http_handlers_keep_existing_behavior() {
    for network in NON_MAINNET_NETWORKS {
        let fixture = Fixture::new(network, None).await;
        let server = HttpServer::new(fixture.state.clone()).await;
        for (path, body) in lightning_requests() {
            let response = server.request(path, body.as_ref()).await;
            if matches!(path, "/decodelninvoice" | "/decodeswapstring") {
                assert_eq!(
                    response.status(),
                    reqwest::StatusCode::OK,
                    "{network:?}: {path}"
                );
            } else {
                assert_http_error(response, "LockedNode", path).await;
            }
        }
        for (path, body, expected) in [
            (
                "/decodelninvoice",
                json!({"invoice": "invalid"}),
                "InvalidInvoice",
            ),
            (
                "/decodeswapstring",
                json!({"swapstring": "invalid"}),
                "InvalidSwapString",
            ),
            (
                "/getchannelid",
                json!({"temporary_channel_id": "invalid"}),
                "InvalidChannelID",
            ),
        ] {
            assert_http_error(server.request(path, Some(&body)).await, expected, path).await;
        }
    }
}

async fn sdk_lightning_results(state: Arc<AppState>) -> Vec<(&'static str, Result<(), APIError>)> {
    let mut results = vec![];
    macro_rules! call {
        ($name:ident $(, $argument:expr)* $(,)?) => {
            results.push((stringify!($name), sdk::$name(state.clone(), $($argument),*).await.map(|_| ())))
        };
    }
    call!(
        async_order_new,
        AsyncOrderNewRequest {
            host_node_id: PUBKEY.into(),
            username: None,
            domain: None
        }
    );
    call!(
        async_order_outbound_invoice,
        serde_json::from_value::<AsyncOrderOutboundInvoiceRequest>(async_order_request()).unwrap()
    );
    call!(
        cancel_hodl_invoice,
        sdk::CancelHodlInvoiceRequestData {
            payment_hash: "01".repeat(32)
        }
    );
    call!(
        claim_hodl_invoice,
        sdk::ClaimHodlInvoiceRequestData {
            payment_hash: "01".repeat(32),
            payment_preimage: "02".repeat(32)
        }
    );
    call!(
        close_channel,
        sdk::CloseChannelRequestData {
            channel_id: "01".repeat(32),
            peer_pubkey: PUBKEY.into(),
            force: false
        }
    );
    call!(connect_peer, format!("{PUBKEY}@127.0.0.1:9735"));
    call!(decode_ln_invoice, ln_invoice());
    call!(
        disconnect_peer,
        sdk::DisconnectPeerRequestData {
            peer_pubkey: PUBKEY.into()
        }
    );
    call!(get_channel_id, "01".repeat(32));
    call!(get_payment, "01".repeat(32), sdk::PaymentType::Outbound);
    call!(get_swap, "01".repeat(32), false);
    call!(invoice_status, ln_invoice());
    call!(
        keysend,
        sdk::KeysendRequestData {
            dest_pubkey: PUBKEY.into(),
            amt_msat: 3000000,
            asset_id: None,
            asset_amount: None
        }
    );
    call!(list_channels);
    call!(list_payments);
    call!(list_peers);
    call!(list_swaps);
    call!(
        create_ln_invoice,
        None,
        900,
        None,
        None,
        None,
        None,
        None,
        None
    );
    call!(
        maker_execute,
        sdk::MakerExecuteRequestData {
            swapstring: swapstring(),
            payment_secret: "01".repeat(32),
            taker_pubkey: PUBKEY.into()
        }
    );
    call!(
        maker_init,
        sdk::MakerInitRequestData {
            qty_from: 1,
            qty_to: 2,
            from_asset: None,
            to_asset: Some(ASSET.into()),
            timeout_sec: 900
        }
    );
    call!(
        open_channel,
        sdk::OpenChannelRequestData {
            peer_pubkey_and_opt_addr: PUBKEY.into(),
            capacity_sat: 100000,
            push_msat: 0,
            public: false,
            with_anchors: true,
            fee_base_msat: None,
            fee_proportional_millionths: None,
            temporary_channel_id: None,
            asset_id: None,
            asset_amount: None,
            push_asset_amount: None,
            virtual_open_mode: None,
        }
    );
    call!(
        send_onion_message,
        sdk::SendOnionMessageRequestData {
            node_ids: vec![PUBKEY.into()],
            tlv_type: 64,
            data: "00".into()
        }
    );
    call!(
        send_payment,
        sdk::SendPaymentRequestData {
            invoice: ln_invoice(),
            amt_msat: None,
            asset_id: None,
            asset_amount: None
        }
    );
    call!(
        taker,
        sdk::TakerRequestData {
            swapstring: swapstring()
        }
    );
    results
}

#[tokio::test]
async fn mainnet_rejects_every_sdk_lightning_entry_point_before_accessing_wallet_state() {
    let fixture = Fixture::new(BitcoinNetwork::Mainnet, None).await;
    // A network rejection must not even wait for the wallet mutex or its locked/unlocked state.
    let guard = fixture.state.unlocked_app_state.lock().await;
    let results = tokio::time::timeout(
        Duration::from_secs(5),
        sdk_lightning_results(fixture.state.clone()),
    )
    .await
    .unwrap();
    for (name, result) in results {
        let error = result.expect_err(name);
        assert!(
            matches!(error, APIError::LightningUnsupportedOnMainnet),
            "{name}: {error:?}"
        );
        assert_eq!(error.to_string(), MESSAGE, "{name}");
        assert_eq!(error.name(), ERROR_NAME, "{name}");
    }
    drop(guard);
    fixture.assert_untouched().await;
}

#[tokio::test]
async fn non_mainnet_sdk_entry_points_keep_existing_behavior() {
    for network in NON_MAINNET_NETWORKS {
        let fixture = Fixture::new(network, None).await;
        for (name, result) in sdk_lightning_results(fixture.state.clone()).await {
            if name == "decode_ln_invoice" {
                assert!(result.is_ok(), "{network:?}: {name}");
            } else {
                assert!(
                    matches!(result, Err(APIError::LockedNode)),
                    "{network:?}: {name}: {result:?}"
                );
            }
        }
        assert!(matches!(
            sdk::decode_ln_invoice(fixture.state.clone(), "invalid".into()).await,
            Err(APIError::InvalidInvoice(_))
        ));
        assert!(matches!(
            sdk::get_channel_id(fixture.state.clone(), "invalid".into()).await,
            Err(APIError::InvalidChannelID)
        ));
    }
}

#[tokio::test]
async fn mainnet_http_authentication_and_json_validation_still_precede_handlers() {
    let keypair = biscuit_auth::KeyPair::new();
    let fixture = Fixture::new(BitcoinNetwork::Mainnet, Some(keypair.public())).await;
    let server = HttpServer::new(fixture.state.clone()).await;
    assert_eq!(
        server.request("/listchannels", None).await.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let token = biscuit_auth::macros::biscuit!("role(\"admin\");")
        .build(&keypair)
        .unwrap()
        .to_base64()
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let response = client
        .get(format!("{}/listchannels", server.url))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_http_error(response, ERROR_NAME, "/listchannels").await;
    let response = client
        .post(format!("{}/lninvoice", server.url))
        .bearer_auth(&token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_http_error(response, "InvalidRequest", "/lninvoice").await;
    let read_only = biscuit_auth::macros::biscuit!("role(\"read-only\");")
        .build(&keypair)
        .unwrap()
        .to_base64()
        .unwrap();
    let response = client
        .post(format!("{}/lninvoice", server.url))
        .bearer_auth(&read_only)
        .json(&json!({"expiry_sec": 900}))
        .send()
        .await
        .unwrap();
    assert_http_error(response, "Forbidden", "/lninvoice").await;
}

#[tokio::test]
async fn mainnet_rgb_invoice_decoding_and_onchain_requirements_are_unchanged() {
    let fixture = Fixture::new(BitcoinNetwork::Mainnet, None).await;
    let server = HttpServer::new(fixture.state.clone()).await;
    let recipient = rgb_lib::utils::recipient_id_from_script_buf(
        ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([1; 20])),
        BitcoinNetwork::Mainnet,
    );
    let invoice = format!("rgb:~/~/~/{recipient}");
    let decoded = sdk::decode_rgb_invoice(fixture.state.clone(), invoice.clone())
        .await
        .unwrap();
    assert_eq!(decoded.recipient_id, recipient);
    assert_eq!(decoded.network, BitcoinNetwork::Mainnet);
    let response = server
        .request("/decodergbinvoice", Some(&json!({"invoice": invoice})))
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["recipient_id"],
        recipient
    );
    for (path, body) in [
        ("/address", Some(json!({}))),
        (
            "/assetlink",
            Some(
                json!({"parent_asset_id": ASSET, "child_asset_id": ASSET, "min_confirmations": 1}),
            ),
        ),
        (
            "/rgbinvoice",
            Some(json!({"min_confirmations": 1, "witness": true, "transport_endpoints": []})),
        ),
        (
            "/sendbtc",
            Some(json!({"amount": 1000, "address": "invalid", "fee_rate": 1, "skip_sync": false})),
        ),
        (
            "/sendrgb",
            Some(
                json!({"donation": false, "fee_rate": 1, "min_confirmations": 1, "recipient_map": {}}),
            ),
        ),
        ("/nodeinfo", None),
        ("/networkinfo", None),
    ] {
        assert_http_error(
            server.request(path, body.as_ref()).await,
            "LockedNode",
            path,
        )
        .await;
    }
    macro_rules! locked {
        ($name:ident $(, $argument:expr)* $(,)?) => {
            assert!(matches!(sdk::$name(fixture.state.clone(), $($argument),*).await, Err(APIError::LockedNode)), stringify!($name))
        };
    }
    locked!(address);
    locked!(node_info);
    locked!(network_info);
    locked!(sign_message, "test".into());
    locked!(verify_message, "test".into(), "invalid".into());
    locked!(
        asset_link,
        AssetLinkRequest {
            parent_asset_id: ASSET.into(),
            child_asset_id: ASSET.into(),
            min_confirmations: 1
        }
    );
    locked!(
        rgb_invoice,
        sdk::RgbInvoiceRequestData {
            asset_id: None,
            assignment_kind: None,
            assignment_amount: None,
            duration_seconds: None,
            min_confirmations: 1,
            witness: true
        }
    );
    locked!(
        send_btc,
        sdk::SendBtcRequestData {
            amount: 1000,
            address: "invalid".into(),
            fee_rate: 1,
            skip_sync: false
        }
    );
    locked!(send_rgb, Default::default(), false, 1, 1);
    assert!(matches!(
        sdk::send_rgb_from_groups(
            fixture.state.clone(),
            sdk::SendRgbRequestData {
                donation: false,
                fee_rate: 1,
                min_confirmations: 1,
                recipient_groups: vec![],
            }
        )
        .await,
        Err(APIError::InvalidAmount(_))
    ));
    fixture.assert_untouched().await;
}
