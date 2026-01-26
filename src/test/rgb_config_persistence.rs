use std::fs;
use tempfile::TempDir;
use crate::database::DatabaseManager;

const INDEXER_URL_FNAME: &str = "indexer_url";
const PROXY_ENDPOINT_FNAME: &str = "proxy_endpoint";
const BITCOIN_NETWORK_FNAME: &str = "bitcoin_network";
const WALLET_FINGERPRINT_FNAME: &str = "wallet_fingerprint";
const WALLET_ACCOUNT_XPUB_COLORED_FNAME: &str = "wallet_account_xpub_colored";
const WALLET_ACCOUNT_XPUB_VANILLA_FNAME: &str = "wallet_account_xpub_vanilla";
const WALLET_MASTER_FINGERPRINT_FNAME: &str = "wallet_master_fingerprint";

#[tokio::test]
async fn test_save_and_load_rgb_config() {
    // Create a temporary database
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Test saving and loading indexer_url
    let indexer_url = "127.0.0.1:50001";
    db_manager.save_rgb_config("indexer_url", indexer_url).await.unwrap();
    let loaded = db_manager.load_rgb_config("indexer_url").await.unwrap();
    assert_eq!(loaded, Some(indexer_url.to_string()));

    // Test saving and loading proxy_endpoint
    let proxy_endpoint = "rpc://127.0.0.1:3000/json-rpc";
    db_manager.save_rgb_config("proxy_endpoint", proxy_endpoint).await.unwrap();
    let loaded = db_manager.load_rgb_config("proxy_endpoint").await.unwrap();
    assert_eq!(loaded, Some(proxy_endpoint.to_string()));

    // Test saving and loading bitcoin_network
    let bitcoin_network = "regtest";
    db_manager.save_rgb_config("bitcoin_network", bitcoin_network).await.unwrap();
    let loaded = db_manager.load_rgb_config("bitcoin_network").await.unwrap();
    assert_eq!(loaded, Some(bitcoin_network.to_string()));

    // Test saving and loading wallet_fingerprint
    let wallet_fingerprint = "fingerprint123";
    db_manager.save_rgb_config("wallet_fingerprint", wallet_fingerprint).await.unwrap();
    let loaded = db_manager.load_rgb_config("wallet_fingerprint").await.unwrap();
    assert_eq!(loaded, Some(wallet_fingerprint.to_string()));

    // Test saving and loading wallet_account_xpub_colored
    let wallet_account_xpub_colored = "tpubD6NzVbkrYhZ4Xferm7Pz4VnjdcDPFyyN2h2kyXJsqJcK8Zz5yVzJAGqFqWyYSyMqvhzKQHQdD8A8JFYGKjzG8VzWJdK8BfMiHdF8J4gHh";
    db_manager.save_rgb_config("wallet_account_xpub_colored", wallet_account_xpub_colored).await.unwrap();
    let loaded = db_manager.load_rgb_config("wallet_account_xpub_colored").await.unwrap();
    assert_eq!(loaded, Some(wallet_account_xpub_colored.to_string()));

    // Test saving and loading wallet_account_xpub_vanilla
    let wallet_account_xpub_vanilla = "tpubD6NzVbkrYhZ4Xferm7Pz4VnjdcDPFyyN2h2kyXJsqJcK8Zz5yVzJAGqFqWyYSyMqvhzKQHQdD8A8JFYGKjzG8VzWJdK8BfMiHdF8J4gHh";
    db_manager.save_rgb_config("wallet_account_xpub_vanilla", wallet_account_xpub_vanilla).await.unwrap();
    let loaded = db_manager.load_rgb_config("wallet_account_xpub_vanilla").await.unwrap();
    assert_eq!(loaded, Some(wallet_account_xpub_vanilla.to_string()));

    // Test saving and loading wallet_master_fingerprint
    let wallet_master_fingerprint = "master_fingerprint_123";
    db_manager.save_rgb_config("wallet_master_fingerprint", wallet_master_fingerprint).await.unwrap();
    let loaded = db_manager.load_rgb_config("wallet_master_fingerprint").await.unwrap();
    assert_eq!(loaded, Some(wallet_master_fingerprint.to_string()));

    // Test loading non-existent key
    let loaded = db_manager.load_rgb_config("non_existent").await.unwrap();
    assert_eq!(loaded, None);

    // Test updating existing key
    let new_indexer_url = "127.0.0.1:50002";
    db_manager.save_rgb_config("indexer_url", new_indexer_url).await.unwrap();
    let loaded = db_manager.load_rgb_config("indexer_url").await.unwrap();
    assert_eq!(loaded, Some(new_indexer_url.to_string()));
}

#[tokio::test]
async fn test_rgb_config_cache() {
    // Create a temporary database
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Save a value
    let indexer_url = "127.0.0.1:50001";
    db_manager.save_rgb_config("indexer_url", indexer_url).await.unwrap();

    // First load should hit DB and cache
    let loaded1 = db_manager.load_rgb_config("indexer_url").await.unwrap();
    assert_eq!(loaded1, Some(indexer_url.to_string()));

    // Second load should hit cache
    let loaded2 = db_manager.load_rgb_config("indexer_url").await.unwrap();
    assert_eq!(loaded2, Some(indexer_url.to_string()));

    // Verify cache is updated on save
    let new_indexer_url = "127.0.0.1:50002";
    db_manager.save_rgb_config("indexer_url", new_indexer_url).await.unwrap();
    let loaded3 = db_manager.load_rgb_config("indexer_url").await.unwrap();
    assert_eq!(loaded3, Some(new_indexer_url.to_string()));
}

#[tokio::test]
async fn test_sync_rgb_config_to_files() {
    // Create a temporary database and directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Save config values
    let indexer_url = "127.0.0.1:50001";
    let proxy_endpoint = "rpc://127.0.0.1:3000/json-rpc";
    let bitcoin_network = "regtest";
    let wallet_fingerprint = "fingerprint123";
    let wallet_account_xpub_colored = "tpubD6NzVbkrYhZ4Xferm7Pz4VnjdcDPFyyN2h2kyXJsqJcK8Zz5yVzJAGqFqWyYSyMqvhzKQHQdD8A8JFYGKjzG8VzWJdK8BfMiHdF8J4gHh";
    let wallet_account_xpub_vanilla = "tpubD6NzVbkrYhZ4Xferm7Pz4VnjdcDPFyyN2h2kyXJsqJcK8Zz5yVzJAGqFqWyYSyMqvhzKQHQdD8A8JFYGKjzG8VzWJdK8BfMiHdF8J4gHh";
    let wallet_master_fingerprint = "master_fingerprint_123";

    db_manager.save_rgb_config("indexer_url", indexer_url).await.unwrap();
    db_manager.save_rgb_config("proxy_endpoint", proxy_endpoint).await.unwrap();
    db_manager.save_rgb_config("bitcoin_network", bitcoin_network).await.unwrap();
    db_manager.save_rgb_config("wallet_fingerprint", wallet_fingerprint).await.unwrap();
    db_manager.save_rgb_config("wallet_account_xpub_colored", wallet_account_xpub_colored).await.unwrap();
    db_manager.save_rgb_config("wallet_account_xpub_vanilla", wallet_account_xpub_vanilla).await.unwrap();
    db_manager.save_rgb_config("wallet_master_fingerprint", wallet_master_fingerprint).await.unwrap();

    // Sync to files
    db_manager.sync_rgb_config_to_files(temp_dir.path()).await.unwrap();

    // Verify files exist and contain correct content
    let indexer_file = temp_dir.path().join(INDEXER_URL_FNAME);
    let proxy_file = temp_dir.path().join(PROXY_ENDPOINT_FNAME);
    let bitcoin_network_file = temp_dir.path().join(BITCOIN_NETWORK_FNAME);
    let wallet_fingerprint_file = temp_dir.path().join(WALLET_FINGERPRINT_FNAME);
    let wallet_account_xpub_colored_file = temp_dir.path().join(WALLET_ACCOUNT_XPUB_COLORED_FNAME);
    let wallet_account_xpub_vanilla_file = temp_dir.path().join(WALLET_ACCOUNT_XPUB_VANILLA_FNAME);
    let wallet_master_fingerprint_file = temp_dir.path().join(WALLET_MASTER_FINGERPRINT_FNAME);

    assert!(indexer_file.exists());
    assert!(proxy_file.exists());
    assert!(bitcoin_network_file.exists());
    assert!(wallet_fingerprint_file.exists());
    assert!(wallet_account_xpub_colored_file.exists());
    assert!(wallet_account_xpub_vanilla_file.exists());
    assert!(wallet_master_fingerprint_file.exists());

    let indexer_content = fs::read_to_string(&indexer_file).unwrap();
    let proxy_content = fs::read_to_string(&proxy_file).unwrap();
    let bitcoin_network_content = fs::read_to_string(&bitcoin_network_file).unwrap();
    let wallet_fingerprint_content = fs::read_to_string(&wallet_fingerprint_file).unwrap();
    let wallet_account_xpub_colored_content = fs::read_to_string(&wallet_account_xpub_colored_file).unwrap();
    let wallet_account_xpub_vanilla_content = fs::read_to_string(&wallet_account_xpub_vanilla_file).unwrap();
    let wallet_master_fingerprint_content = fs::read_to_string(&wallet_master_fingerprint_file).unwrap();

    assert_eq!(indexer_content.trim(), indexer_url);
    assert_eq!(proxy_content.trim(), proxy_endpoint);
    assert_eq!(bitcoin_network_content.trim(), bitcoin_network);
    assert_eq!(wallet_fingerprint_content.trim(), wallet_fingerprint);
    assert_eq!(wallet_account_xpub_colored_content.trim(), wallet_account_xpub_colored);
    assert_eq!(wallet_account_xpub_vanilla_content.trim(), wallet_account_xpub_vanilla);
    assert_eq!(wallet_master_fingerprint_content.trim(), wallet_master_fingerprint);
}

#[tokio::test]
async fn test_migrate_indexer_url_from_file() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Create a file with indexer_url
    let indexer_url = "127.0.0.1:50001";
    let indexer_file = temp_dir.path().join(INDEXER_URL_FNAME);
    fs::write(&indexer_file, indexer_url).unwrap();

    // Migrate from file to DB
    db_manager.migrate_indexer_url_from_file(temp_dir.path()).await.unwrap();

    // Verify value is in DB
    let loaded = db_manager.load_rgb_config("indexer_url").await.unwrap();
    assert_eq!(loaded, Some(indexer_url.to_string()));

    // Verify file still exists (migration doesn't delete it)
    assert!(indexer_file.exists());
    let file_content = fs::read_to_string(&indexer_file).unwrap();
    assert_eq!(file_content.trim(), indexer_url);
}

#[tokio::test]
async fn test_migrate_no_file_present() {
    // Create a temporary directory without indexer_url file
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Try to migrate when no file exists
    db_manager.migrate_indexer_url_from_file(temp_dir.path()).await.unwrap();

    // Verify no value is set in DB
    let loaded = db_manager.load_rgb_config("indexer_url").await.unwrap();
    assert_eq!(loaded, None);
}

#[tokio::test]
async fn test_sync_empty_config_to_files() {
    // Create a temporary database and directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // No config saved, sync to files
    db_manager.sync_rgb_config_to_files(temp_dir.path()).await.unwrap();

    // Verify no files are created
    let indexer_file = temp_dir.path().join(INDEXER_URL_FNAME);
    let proxy_file = temp_dir.path().join(PROXY_ENDPOINT_FNAME);
    let bitcoin_network_file = temp_dir.path().join(BITCOIN_NETWORK_FNAME);
    let wallet_fingerprint_file = temp_dir.path().join(WALLET_FINGERPRINT_FNAME);
    let wallet_account_xpub_colored_file = temp_dir.path().join(WALLET_ACCOUNT_XPUB_COLORED_FNAME);
    let wallet_account_xpub_vanilla_file = temp_dir.path().join(WALLET_ACCOUNT_XPUB_VANILLA_FNAME);
    let wallet_master_fingerprint_file = temp_dir.path().join(WALLET_MASTER_FINGERPRINT_FNAME);

    assert!(!indexer_file.exists());
    assert!(!proxy_file.exists());
    assert!(!bitcoin_network_file.exists());
    assert!(!wallet_fingerprint_file.exists());
    assert!(!wallet_account_xpub_colored_file.exists());
    assert!(!wallet_account_xpub_vanilla_file.exists());
    assert!(!wallet_master_fingerprint_file.exists());
}

#[tokio::test]
async fn test_sync_partial_config_to_files() {
    // Create a temporary database and directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Save only indexer_url
    let indexer_url = "127.0.0.1:50001";
    db_manager.save_rgb_config("indexer_url", indexer_url).await.unwrap();

    // Sync to files
    db_manager.sync_rgb_config_to_files(temp_dir.path()).await.unwrap();

    // Verify only indexer_url file exists
    let indexer_file = temp_dir.path().join(INDEXER_URL_FNAME);
    let proxy_file = temp_dir.path().join(PROXY_ENDPOINT_FNAME);

    assert!(indexer_file.exists());
    assert!(!proxy_file.exists());

    let indexer_content = fs::read_to_string(&indexer_file).unwrap();
    assert_eq!(indexer_content.trim(), indexer_url);
}

#[tokio::test]
async fn test_overwrite_file_on_sync() {
    // Create a temporary database and directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Create file with old content
    let indexer_file = temp_dir.path().join(INDEXER_URL_FNAME);
    fs::write(&indexer_file, "old.url:9999").unwrap();

    // Save new config and sync
    let new_indexer_url = "127.0.0.1:50001";
    db_manager.save_rgb_config("indexer_url", new_indexer_url).await.unwrap();
    db_manager.sync_rgb_config_to_files(temp_dir.path()).await.unwrap();

    // Verify file content is overwritten
    let indexer_content = fs::read_to_string(&indexer_file).unwrap();
    assert_eq!(indexer_content.trim(), new_indexer_url);
}

#[tokio::test]
async fn test_migrate_bitcoin_network_from_file() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Create a file with bitcoin_network
    let bitcoin_network = "regtest";
    let bitcoin_network_file = temp_dir.path().join(BITCOIN_NETWORK_FNAME);
    fs::write(&bitcoin_network_file, bitcoin_network).unwrap();

    // Migrate from file to DB
    db_manager.migrate_bitcoin_network_from_file(temp_dir.path()).await.unwrap();

    // Verify value is in DB
    let loaded = db_manager.load_rgb_config("bitcoin_network").await.unwrap();
    assert_eq!(loaded, Some(bitcoin_network.to_string()));

    // Verify file still exists (migration doesn't delete it)
    assert!(bitcoin_network_file.exists());
    let file_content = fs::read_to_string(&bitcoin_network_file).unwrap();
    assert_eq!(file_content.trim(), bitcoin_network);
}

#[tokio::test]
async fn test_migrate_wallet_fingerprint_from_file() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Create a file with wallet_fingerprint
    let wallet_fingerprint = "fingerprint123";
    let wallet_fingerprint_file = temp_dir.path().join(WALLET_FINGERPRINT_FNAME);
    fs::write(&wallet_fingerprint_file, wallet_fingerprint).unwrap();

    // Migrate from file to DB
    db_manager.migrate_wallet_fingerprint_from_file(temp_dir.path()).await.unwrap();

    // Verify value is in DB
    let loaded = db_manager.load_rgb_config("wallet_fingerprint").await.unwrap();
    assert_eq!(loaded, Some(wallet_fingerprint.to_string()));

    // Verify file still exists (migration doesn't delete it)
    assert!(wallet_fingerprint_file.exists());
    let file_content = fs::read_to_string(&wallet_fingerprint_file).unwrap();
    assert_eq!(file_content.trim(), wallet_fingerprint);
}

#[tokio::test]
async fn test_migrate_wallet_account_xpub_colored_from_file() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Create a file with wallet_account_xpub_colored
    let wallet_account_xpub_colored = "tpubD6NzVbkrYhZ4Xferm7Pz4VnjdcDPFyyN2h2kyXJsqJcK8Zz5yVzJAGqFqWyYSyMqvhzKQHQdD8A8JFYGKjzG8VzWJdK8BfMiHdF8J4gHh";
    let wallet_account_xpub_colored_file = temp_dir.path().join(WALLET_ACCOUNT_XPUB_COLORED_FNAME);
    fs::write(&wallet_account_xpub_colored_file, wallet_account_xpub_colored).unwrap();

    // Migrate from file to DB
    db_manager.migrate_wallet_account_xpub_colored_from_file(temp_dir.path()).await.unwrap();

    // Verify value is in DB
    let loaded = db_manager.load_rgb_config("wallet_account_xpub_colored").await.unwrap();
    assert_eq!(loaded, Some(wallet_account_xpub_colored.to_string()));

    // Verify file still exists (migration doesn't delete it)
    assert!(wallet_account_xpub_colored_file.exists());
    let file_content = fs::read_to_string(&wallet_account_xpub_colored_file).unwrap();
    assert_eq!(file_content.trim(), wallet_account_xpub_colored);
}

#[tokio::test]
async fn test_migrate_wallet_account_xpub_vanilla_from_file() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Create a file with wallet_account_xpub_vanilla
    let wallet_account_xpub_vanilla = "tpubD6NzVbkrYhZ4Xferm7Pz4VnjdcDPFyyN2h2kyXJsqJcK8Zz5yVzJAGqFqWyYSyMqvhzKQHQdD8A8JFYGKjzG8VzWJdK8BfMiHdF8J4gHh";
    let wallet_account_xpub_vanilla_file = temp_dir.path().join(WALLET_ACCOUNT_XPUB_VANILLA_FNAME);
    fs::write(&wallet_account_xpub_vanilla_file, wallet_account_xpub_vanilla).unwrap();

    // Migrate from file to DB
    db_manager.migrate_wallet_account_xpub_vanilla_from_file(temp_dir.path()).await.unwrap();

    // Verify value is in DB
    let loaded = db_manager.load_rgb_config("wallet_account_xpub_vanilla").await.unwrap();
    assert_eq!(loaded, Some(wallet_account_xpub_vanilla.to_string()));

    // Verify file still exists (migration doesn't delete it)
    assert!(wallet_account_xpub_vanilla_file.exists());
    let file_content = fs::read_to_string(&wallet_account_xpub_vanilla_file).unwrap();
    assert_eq!(file_content.trim(), wallet_account_xpub_vanilla);
}

#[tokio::test]
async fn test_migrate_wallet_master_fingerprint_from_file() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let db_manager = DatabaseManager::new(&db_path).await.unwrap();

    // Create a file with wallet_master_fingerprint
    let wallet_master_fingerprint = "master_fingerprint_123";
    let wallet_master_fingerprint_file = temp_dir.path().join(WALLET_MASTER_FINGERPRINT_FNAME);
    fs::write(&wallet_master_fingerprint_file, wallet_master_fingerprint).unwrap();

    // Migrate from file to DB
    db_manager.migrate_wallet_master_fingerprint_from_file(temp_dir.path()).await.unwrap();

    // Verify value is in DB
    let loaded = db_manager.load_rgb_config("wallet_master_fingerprint").await.unwrap();
    assert_eq!(loaded, Some(wallet_master_fingerprint.to_string()));

    // Verify file still exists (migration doesn't delete it)
    assert!(wallet_master_fingerprint_file.exists());
    let file_content = fs::read_to_string(&wallet_master_fingerprint_file).unwrap();
    assert_eq!(file_content.trim(), wallet_master_fingerprint);
}
