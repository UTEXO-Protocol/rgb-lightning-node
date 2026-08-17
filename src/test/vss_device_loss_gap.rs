//! Deterministic test for the device-loss durability gap of funding-critical
//! RGB state replicated through the best-effort path (`SyncedKvStore`).
//!
//! Expected to fail on current `dev` and pass once funding-critical
//! namespaces require remote durability (or are recoverable another way).

#[cfg(feature = "vss")]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use bitcoin::secp256k1::{rand::rngs::OsRng, Secp256k1, SecretKey};
    use hex::DisplayHex;
    use lightning::rgb_utils::{RGB_CHANNEL_INFO_NS, RGB_PRIMARY_NS};
    use lightning::util::persist::{
        KVStoreSync, CHANNEL_MANAGER_PERSISTENCE_KEY,
        CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
    };
    use sea_orm::{ConnectOptions, Database};
    use tempfile::TempDir;

    use crate::kv_store::SeaOrmKvStore;
    use crate::synced_kv_store::SyncedKvStore;
    use crate::vss_kv_store::VssKvStore;

    const VSS_URL: &str = "http://localhost:8081/vss";

    /// Bound the wait for the outage-time write. It must comfortably exceed the
    /// VSS retry budget (`retry_max_total_delay_secs`, 5s by default) so a
    /// best-effort write has deterministically returned by the deadline, while
    /// a fail-closed *blocking* write is still parked and gets unblocked below.
    const OUTAGE_WRITE_WAIT: Duration = Duration::from_secs(15);

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

    fn open_sqlite(dir: &std::path::Path) -> Arc<sea_orm::DatabaseConnection> {
        use rln_migration::MigratorTrait;

        let db_path = dir.join("test_rln_db");
        let conn_str = format!("sqlite:{}?mode=rwc", db_path.display());
        let mut opt = ConnectOptions::new(conn_str);
        opt.max_connections(1)
            .connect_timeout(Duration::from_secs(5));
        let db = crate::runtime::block_on(Database::connect(opt)).expect("test db");
        crate::runtime::block_on(rln_migration::Migrator::up(&db, None)).expect("migration");
        Arc::new(db)
    }

    /// A funding-critical mutation acknowledged to the caller must survive a
    /// device-loss restore. The single invariant checked is: `!acked ||
    /// survived` — a write the caller was told succeeded must be readable from
    /// VSS after the local store is wiped. On `dev`, RGB channel info is
    /// replicated best-effort, so an outage-time write acks yet is only queued
    /// locally, and the restored node holds a channel with no RGB mapping.
    ///
    /// Both correct fixes pass unchanged: fail-closed (the write returns `Err`,
    /// so `acked` is false) or durable (the write blocks until VSS is back and
    /// the value survives).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acked_rgb_channel_info_must_survive_device_loss() {
        if !vss_server_available() {
            eprintln!("SKIP: VSS server not available at {VSS_URL}");
            return;
        }

        let proxy = super::super::vss_offline_force_close::VssProxy::start();
        let (signing_key, store_id) = generate_test_keys();

        // Held for the test's lifetime so the SQLite files are not removed
        // mid-run; dropped (and cleaned up) at the end.
        let dir = TempDir::new().expect("tempdir");
        let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(dir.path())));
        let remote = Arc::new(
            VssKvStore::new(proxy.url(), store_id.clone(), signing_key).expect("vss store"),
        );
        let synced = Arc::new(SyncedKvStore::with_vss(local, remote));

        // Baseline: a manager write while VSS is up must be durably on VSS
        // before we proceed, so the post-restore manager assertion can only
        // fail for the reason under test.
        synced
            .write(
                CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
                CHANNEL_MANAGER_PERSISTENCE_KEY,
                vec![0xCA; 64],
            )
            .expect("manager write");
        assert_eq!(
            synced.pending_remote_writes(),
            0,
            "manager write must be durable on VSS before the outage begins"
        );

        // Outage write of the funding-critical RGB mapping.
        proxy.go_offline();
        let writer = {
            let synced = Arc::clone(&synced);
            std::thread::spawn(move || {
                synced.write(
                    RGB_PRIMARY_NS,
                    RGB_CHANNEL_INFO_NS,
                    "chan_1",
                    b"rgb_info".to_vec(),
                )
            })
        };
        let deadline = Instant::now() + OUTAGE_WRITE_WAIT;
        while !writer.is_finished() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // A fail-closed *blocking* write is still parked: let VSS return so it
        // can complete (and become durable). A best-effort write has already
        // returned by now.
        if !writer.is_finished() {
            proxy.go_online();
        }
        let acked = writer.join().expect("writer thread").is_ok();

        // Device loss: the local store is gone, only VSS survives.
        drop(synced);

        let fresh_dir = TempDir::new().expect("fresh tempdir");
        let fresh_local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(
            fresh_dir.path(),
        )));
        let fresh_remote = Arc::new(
            VssKvStore::new(VSS_URL.to_string(), store_id, signing_key).expect("vss store"),
        );
        let restored = SyncedKvStore::with_vss(Arc::clone(&fresh_local), fresh_remote);
        restored.restore_from_vss(true).expect("restore");

        assert_eq!(
            restored
                .read(
                    CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_KEY,
                )
                .expect("manager must be restored"),
            vec![0xCA; 64],
        );

        let restored_rgb = restored
            .read(RGB_PRIMARY_NS, RGB_CHANNEL_INFO_NS, "chan_1")
            .ok();
        assert!(
            !acked || restored_rgb.as_deref() == Some(b"rgb_info".as_slice()),
            "an acknowledged funding-critical RGB mapping must survive device loss; \
             the restored node holds LDK state for a channel it has no RGB mapping for"
        );
    }
}
