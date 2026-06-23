//! Unit tests for the remote-first async `KVStore` (`RemoteFirstKvStore`).
//!
//! Requires a running VSS server: `docker compose --profile vss up -d`
//! Run with: `cargo test --features vss remote_first_kv -- --nocapture`

#[cfg(feature = "vss")]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bitcoin::secp256k1::{rand::rngs::OsRng, Secp256k1, SecretKey};
    use hex::DisplayHex;
    use lightning::util::persist::{KVStore, KVStoreSync};
    use sea_orm::{ConnectOptions, Database};

    use crate::async_kv_store::RemoteFirstKvStore;
    use crate::kv_store::SeaOrmKvStore;
    use crate::vss_kv_store::VssKvStore;

    const VSS_URL: &str = "http://localhost:8081/vss";

    fn generate_test_keys() -> (SecretKey, String) {
        let secp = Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut OsRng);
        let store_id = format!("rln_test_{}", public_key.serialize()[0..8].as_hex());
        (secret_key, store_id)
    }

    fn vss_server_available() -> bool {
        std::net::TcpStream::connect_timeout(
            &"127.0.0.1:8081".parse().unwrap(),
            Duration::from_secs(2),
        )
        .is_ok()
    }

    fn create_test_sqlite() -> Arc<sea_orm::DatabaseConnection> {
        use rln_migration::MigratorTrait;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.keep();
        let db_path = path.join("test_rln_db");
        let conn_str = format!("sqlite:{}?mode=rwc", db_path.display());
        let mut opt = ConnectOptions::new(conn_str);
        opt.max_connections(1)
            .connect_timeout(Duration::from_secs(5));
        let db = crate::runtime::block_on(Database::connect(opt)).expect("test db");
        crate::runtime::block_on(rln_migration::Migrator::up(&db, None)).expect("migration");
        Arc::new(db)
    }

    /// A successful write must land durably in VSS *and* be mirrored locally,
    /// and be readable back through the store.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_first_write_is_durable_in_vss() {
        if !vss_server_available() {
            eprintln!("SKIP: VSS server not available at {VSS_URL}");
            return;
        }

        let (signing_key, store_id) = generate_test_keys();
        let local = Arc::new(SeaOrmKvStore::from_connection(create_test_sqlite()));
        let remote = Arc::new(
            VssKvStore::new(VSS_URL.to_string(), store_id.clone(), signing_key).expect("vss store"),
        );
        let store = RemoteFirstKvStore::new(Arc::clone(&local), Some(Arc::clone(&remote)));

        let (ns, sub, key) = ("monitors", "", "chan1");
        let data = b"monitor-state-v1".to_vec();

        store
            .write(ns, sub, key, data.clone())
            .await
            .expect("write");

        // Durable in VSS: a fresh client over the same store_id reads it back.
        let fresh = VssKvStore::new(VSS_URL.to_string(), store_id, signing_key).expect("fresh vss");
        assert_eq!(
            fresh.read_async(ns, sub, key).await.expect("vss read"),
            data,
            "value must be durable in VSS after write returns"
        );

        // Mirrored locally for fast reads.
        assert_eq!(
            KVStoreSync::read(&*local, ns, sub, key).expect("local read"),
            data,
            "value must be mirrored to the local store"
        );

        // Readable through the store.
        assert_eq!(store.read(ns, sub, key).await.expect("read"), data);
    }

    /// If the VSS write fails, the whole write must resolve `Err` — never a
    /// silent local-only `Ok`. The local store must be left untouched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_first_write_fails_when_vss_unreachable() {
        let (signing_key, store_id) = generate_test_keys();
        let local = Arc::new(SeaOrmKvStore::from_connection(create_test_sqlite()));
        // Point the remote at a closed port so every VSS write errors.
        let remote = Arc::new(
            VssKvStore::new("http://127.0.0.1:1/vss".to_string(), store_id, signing_key)
                .expect("vss store"),
        );
        let store = RemoteFirstKvStore::new(Arc::clone(&local), Some(remote));

        let (ns, sub, key) = ("monitors", "", "chan1");

        let err = store
            .write(ns, sub, key, b"should-not-persist".to_vec())
            .await
            .expect_err("write must fail when VSS is unreachable");
        eprintln!("expected write failure: {err}");

        // No silent local-only write: local must still be empty for this key.
        let local_read = KVStoreSync::read(&*local, ns, sub, key);
        assert!(
            matches!(&local_read, Err(e) if e.kind() == bitcoin::io::ErrorKind::NotFound),
            "local store must not contain the key after a failed remote-first write, got {local_read:?}"
        );
    }
}
