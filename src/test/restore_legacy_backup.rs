use super::*;

use rln_migration::{Migrator, MigratorTrait};

use crate::backup::do_backup;
use crate::database::RlnDatabase;

const TEST_DIR_BASE: &str = "tmp/restore_legacy_backup/";

const PASSWORD: &str = "password123";
// Mnemonic record written by the magic-crypt encryption used before scrypt and
// XChaCha20Poly1305, encrypted with PASSWORD.
const LEGACY_MNEMONIC_RECORD: &str = "m6g98F3pqJ49njHK+XWpIBUKEuxv2Gy6Qlt8S900rkc7FA4aMG3hfRAUYYEOJfdUtwqDnImV8W7Rdy6zLcY8oBFbdRGdz9kXb5iRY1BPf81gPX0OEa7B4Cn/dsNNCMJ1";

// Build a wallet dir holding a mnemonic this version can no longer decrypt, then back it up.
fn legacy_backup(wallet_dir: &str, backup_path: &str) {
    if Path::new(wallet_dir).exists() {
        std::fs::remove_dir_all(wallet_dir).unwrap();
    }
    std::fs::create_dir_all(wallet_dir).unwrap();
    let connection_string = format!("sqlite:{wallet_dir}/rln_db?mode=rwc");
    let db = crate::runtime::block_on(Database::connect(ConnectOptions::new(connection_string)))
        .expect("db connection");
    crate::runtime::block_on(Migrator::up(&db, None)).expect("run migrations");
    RlnDatabase::new(db.clone())
        .save_mnemonic(LEGACY_MNEMONIC_RECORD.to_string())
        .expect("save legacy mnemonic");
    // flush any WAL, so the backed up files hold the record
    crate::runtime::block_on(db.close()).expect("close db");

    if Path::new(backup_path).exists() {
        std::fs::remove_file(backup_path).unwrap();
    }
    do_backup(Path::new(wallet_dir), Path::new(backup_path), PASSWORD).expect("backup");
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn restore_legacy_backup_leaves_the_node_initializable() {
    initialize();

    let wallet_dir = format!("{TEST_DIR_BASE}legacy_wallet");
    let backup_path = format!("{TEST_DIR_BASE}legacy_backup");
    legacy_backup(&wallet_dir, &backup_path);

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let node1_addr = start_daemon(&test_dir_node1, NODE1_PEER_PORT, None, false).await;

    // the backup decrypts with the given password, but its mnemonic record does not
    let payload = RestoreRequest {
        backup_path: backup_path.clone(),
        password: PASSWORD.to_string(),
    };
    let res = reqwest::Client::new()
        .post(format!("http://{node1_addr}/restore"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    check_response_is_nok(
        res,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "The stored mnemonic is corrupted",
        "CorruptedMnemonic",
    )
    .await;

    // the failed restore must leave the node initializable through the API alone
    init(node1_addr, PASSWORD, None).await;
}
