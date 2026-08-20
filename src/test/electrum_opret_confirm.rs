use super::*;

// Regression test for a `lightning-transaction-sync` bug: its electrum client ignores the
// `script_pubkey` passed to `Filter::register_tx` and instead derives the script whose history it
// queries from the transaction's *first* output. Indexers do not track provably-unspendable
// outputs, so a transaction whose first output is an OP_RETURN -- which is what an RGB `opret`
// commitment in a channel funding transaction looks like -- is never reported as confirmed, and
// the channel never reaches `channel_ready`.

#[derive(Default)]
struct ConfirmSpy {
    confirmed: Mutex<Vec<Txid>>,
}

impl Confirm for ConfirmSpy {
    fn transactions_confirmed(&self, _header: &Header, txdata: &TransactionData, _height: u32) {
        let mut confirmed = self.confirmed.lock().unwrap();
        for (_, tx) in txdata {
            confirmed.push(tx.compute_txid());
        }
    }
    fn transaction_unconfirmed(&self, _txid: &Txid) {}
    fn best_block_updated(&self, _header: &Header, _height: u32) {}
    fn get_relevant_txids(&self) -> Vec<(Txid, u32, Option<BlockHash>)> {
        vec![]
    }
}

// broadcasts and confirms a transaction whose first output is an OP_RETURN, returning its txid and
// the scriptPubKey of that first output
fn send_opret_tx() -> (Txid, ScriptBuf) {
    let address = bitcoind(&["-rpcwallet=miner", "getnewaddress"]);
    let outputs = format!(
        r#"[{{"data":"{}"}},{{"{address}":0.001}}]"#,
        "de".repeat(32)
    );
    let funded = bitcoind(&[
        "-rpcwallet=miner",
        "walletcreatefundedpsbt",
        "[]",
        &outputs,
        "0",
        // bitcoind inserts the change output at a random position by default, which would leave
        // the OP_RETURN somewhere other than the first output: pin change last instead
        r#"{"fee_rate":5,"changePosition":2}"#,
    ]);
    let psbt = serde_json::from_str::<serde_json::Value>(&funded).unwrap()["psbt"]
        .as_str()
        .unwrap()
        .to_string();
    let processed = bitcoind(&["-rpcwallet=miner", "walletprocesspsbt", &psbt]);
    let processed_psbt = serde_json::from_str::<serde_json::Value>(&processed).unwrap()["psbt"]
        .as_str()
        .unwrap()
        .to_string();
    let finalized = bitcoind(&["-rpcwallet=miner", "finalizepsbt", &processed_psbt]);
    let raw = serde_json::from_str::<serde_json::Value>(&finalized).unwrap()["hex"]
        .as_str()
        .unwrap()
        .to_string();
    let txid =
        Txid::from_str(&bitcoind(&["-rpcwallet=miner", "sendrawtransaction", &raw])).unwrap();
    bitcoind(&["-rpcwallet=miner", "-generate", "6"]);

    let tx: BitcoinTransaction = encode::deserialize(&hex_str_to_vec(&raw).unwrap()).unwrap();
    let first_script = tx.output.first().unwrap().script_pubkey.clone();
    assert!(
        first_script.is_op_return(),
        "the first output must be the OP_RETURN for this test to mean anything"
    );
    (txid, first_script)
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn opret_first_output_still_confirms() {
    initialize();

    let (txid, first_script) = send_opret_tx();

    // the indexer does not track the OP_RETURN, so resolving the transaction through that output
    // cannot work -- this is the precondition that makes the bug bite
    let probe = electrum_client::Client::new(ELECTRUM_URL_REGTEST).unwrap();
    let history = probe.script_get_history(&first_script).unwrap();
    assert!(
        history.is_empty(),
        "expected the indexer to have no history for the OP_RETURN output, got {history:?}"
    );

    let logger = Arc::new(FilesystemLogger::new(PathBuf::from(
        "tmp/electrum_opret_confirm",
    )));
    let sync_client = ElectrumSyncClient::new(ELECTRUM_URL_REGTEST.to_string(), logger).unwrap();
    sync_client.register_tx(&txid, &first_script);

    let spy = Arc::new(ConfirmSpy::default());
    let confirmable: Arc<dyn Confirm + Send + Sync> = spy.clone();
    tokio::task::spawn_blocking(move || sync_client.sync(vec![confirmable]).unwrap())
        .await
        .unwrap();

    let confirmed = spy.confirmed.lock().unwrap().clone();
    assert!(
        confirmed.contains(&txid),
        "transaction with an OP_RETURN first output was never reported as confirmed"
    );
}

// broadcasts and confirms a transaction whose outputs are *all* OP_RETURNs, returning its txid and
// the scriptPubKey of the first output. The whole input value is paid as fee, which is what lets
// the transaction have no spendable output at all.
fn send_all_opret_tx() -> (Txid, ScriptBuf) {
    // a dedicated small UTXO to burn as fee, so the transaction can have no change output
    let address = bitcoind(&["-rpcwallet=miner", "getnewaddress"]);
    let funding_txid = bitcoind(&["-rpcwallet=miner", "sendtoaddress", &address, "0.0001"]);
    bitcoind(&["-rpcwallet=miner", "-generate", "1"]);

    let unspents = bitcoind(&[
        "-rpcwallet=miner",
        "listunspent",
        "1",
        "9999999",
        &format!(r#"["{address}"]"#),
    ]);
    let unspent = serde_json::from_str::<serde_json::Value>(&unspents).unwrap()[0].clone();
    assert_eq!(unspent["txid"].as_str().unwrap(), funding_txid);
    let inputs = format!(
        r#"[{{"txid":"{}","vout":{}}}]"#,
        funding_txid,
        unspent["vout"].as_u64().unwrap()
    );
    // bitcoind rejects more than one `data` entry, so the transaction gets a single OP_RETURN
    let outputs = format!(r#"[{{"data":"{}"}}]"#, "de".repeat(32));
    let raw_unsigned = bitcoind(&[
        "-rpcwallet=miner",
        "createrawtransaction",
        &inputs,
        &outputs,
    ]);
    let signed = bitcoind(&[
        "-rpcwallet=miner",
        "signrawtransactionwithwallet",
        &raw_unsigned,
    ]);
    let raw = serde_json::from_str::<serde_json::Value>(&signed).unwrap()["hex"]
        .as_str()
        .unwrap()
        .to_string();
    // the whole input is fee, so the default max-feerate guard has to be disabled
    let txid = Txid::from_str(&bitcoind(&[
        "-rpcwallet=miner",
        "sendrawtransaction",
        &raw,
        "0",
    ]))
    .unwrap();
    bitcoind(&["-rpcwallet=miner", "-generate", "6"]);

    let tx: BitcoinTransaction = encode::deserialize(&hex_str_to_vec(&raw).unwrap()).unwrap();
    assert!(
        tx.output.iter().all(|txo| txo.script_pubkey.is_op_return()),
        "every output must be an OP_RETURN for this test to reach the input fallback"
    );
    (txid, tx.output[0].script_pubkey.clone())
}

// The OP_RETURN filter alone is not enough when *no* output is indexable: the sync client then has
// to fall back to walking the inputs and querying the history of a previous output's script. That
// fallback is what this covers; `opret_first_output_still_confirms` only reaches the filter.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn all_opret_outputs_confirm_via_input_fallback() {
    initialize();

    let (txid, first_script) = send_all_opret_tx();

    let probe = electrum_client::Client::new(ELECTRUM_URL_REGTEST).unwrap();
    let history = probe.script_get_history(&first_script).unwrap();
    assert!(
        history.is_empty(),
        "expected the indexer to have no history for the OP_RETURN output, got {history:?}"
    );

    let logger = Arc::new(FilesystemLogger::new(PathBuf::from(
        "tmp/electrum_opret_confirm_fallback",
    )));
    let sync_client = ElectrumSyncClient::new(ELECTRUM_URL_REGTEST.to_string(), logger).unwrap();
    sync_client.register_tx(&txid, &first_script);

    let spy = Arc::new(ConfirmSpy::default());
    let confirmable: Arc<dyn Confirm + Send + Sync> = spy.clone();
    tokio::task::spawn_blocking(move || sync_client.sync(vec![confirmable]).unwrap())
        .await
        .unwrap();

    let confirmed = spy.confirmed.lock().unwrap().clone();
    assert!(
        confirmed.contains(&txid),
        "transaction with only OP_RETURN outputs was never reported as confirmed"
    );
}
