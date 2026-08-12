//! Deterministic tests for the VSS durability contract of the best-effort
//! replication path (`SyncedKvStore`).
//!
//! Each test encodes an invariant a crash/device-loss recovery relies on.
//! They are expected to fail on current `dev` and pass once the persistence
//! layer is made crash-consistent.

#[cfg(feature = "vss")]
mod tests {
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use bitcoin::secp256k1::{rand::rngs::OsRng, Secp256k1, SecretKey};
    use hex::DisplayHex;
    use lightning::util::persist::KVStoreSync;
    use sea_orm::{ConnectOptions, Database};

    use crate::kv_store::SeaOrmKvStore;
    use crate::synced_kv_store::SyncedKvStore;
    use crate::vss_kv_store::{vss_key, VssKvStore};

    /// Same persisted contract as `SyncedKvStore`'s pending-retry namespace.
    const PENDING_NS: &str = "vss_pending";
    const PENDING_QUEUE_CAP: usize = 1000;

    fn generate_test_keys() -> (SecretKey, String) {
        let secp = Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut OsRng);
        let store_id = format!("rln_test_{}", public_key.serialize()[0..8].as_hex());
        (secret_key, store_id)
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

    fn unreachable_vss() -> Arc<VssKvStore> {
        let (signing_key, store_id) = generate_test_keys();
        Arc::new(
            VssKvStore::new("http://127.0.0.1:5/vss".to_string(), store_id, signing_key)
                .expect("vss store"),
        )
    }

    /// TCP server that accepts VSS requests and holds them open (no response)
    /// until `cut()`, which drops every connection and starts refusing new
    /// ones. Models a remote mutation that is in flight at crash/shutdown
    /// time: sent, not acknowledged.
    struct StallingVss {
        port: u16,
        arrived: mpsc::Receiver<()>,
        cut: Arc<AtomicBool>,
    }

    impl StallingVss {
        fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (tx, arrived) = mpsc::channel();
            let cut = Arc::new(AtomicBool::new(false));
            let cut_bg = Arc::clone(&cut);
            std::thread::spawn(move || {
                let mut held: Vec<std::net::TcpStream> = Vec::new();
                listener
                    .set_nonblocking(true)
                    .expect("nonblocking listener");
                loop {
                    if cut_bg.load(Ordering::Acquire) {
                        return; // drops listener and held streams
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_millis(500)))
                                .ok();
                            let mut buf = [0u8; 512];
                            if stream.read(&mut buf).is_ok() {
                                let _ = tx.send(());
                            }
                            held.push(stream);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => return,
                    }
                }
            });
            Self { port, arrived, cut }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/vss", self.port)
        }

        fn wait_for_request(&self) {
            self.arrived
                .recv_timeout(Duration::from_secs(20))
                .expect("VSS request must arrive");
        }

        fn cut(&self) {
            self.cut.store(true, Ordering::Release);
        }
    }

    /// Copy the SQLite files (db + wal + shm) while the node is "running".
    /// The copy is the exact on-disk image an OS kill at that instant leaves
    /// behind.
    fn snapshot_sqlite(dir: &std::path::Path, dest: &std::path::Path) {
        std::fs::create_dir_all(dest).expect("snapshot dir");
        for suffix in ["", "-wal", "-shm"] {
            let src = dir.join(format!("test_rln_db{suffix}"));
            if src.exists() {
                std::fs::copy(&src, dest.join(format!("test_rln_db{suffix}"))).expect("copy");
            }
        }
    }

    /// A killed process must observe either the complete old state or the
    /// complete new local value plus its durable retry intent. On `dev` the
    /// local value commits first and the retry intent is only persisted after
    /// the remote attempt fails, so a kill while the VSS request is in flight
    /// leaves a crash image whose value will never be replicated: a later
    /// device-loss restore is silently stale.
    #[test]
    fn crash_image_must_retain_replication_intent() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(&dir)));
        let stall = StallingVss::start();
        let (signing_key, store_id) = generate_test_keys();
        let remote =
            Arc::new(VssKvStore::new(stall.url(), store_id, signing_key).expect("vss store"));
        let synced = Arc::new(SyncedKvStore::with_vss(Arc::clone(&local), remote));

        let writer = {
            let synced = Arc::clone(&synced);
            std::thread::spawn(move || synced.write("", "", "aux_state", b"v1".to_vec()))
        };

        // The remote put is in flight: VSS has acknowledged nothing. Snapshot
        // the exact on-disk image an OS kill leaves now.
        stall.wait_for_request();
        let snapshot_dir = dir.join("crash_image");
        snapshot_sqlite(&dir, &snapshot_dir);

        stall.cut();
        let _ = writer.join().expect("writer thread");

        // "Restart" from the crash image. Complete old state is fine; the new
        // local value without a surviving retry intent is not.
        let restarted_local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(&snapshot_dir)));
        let restarted = SyncedKvStore::with_vss(Arc::clone(&restarted_local), unreachable_vss());

        if let Ok(value) = restarted_local.read("", "", "aux_state") {
            assert_eq!(value, b"v1".to_vec());
            assert_eq!(
                restarted.pending_remote_writes(),
                1,
                "crash image holds a local value VSS never acknowledged, but no \
                 retry intent survived: the value will never replicate and a \
                 device-loss restore is silently stale"
            );
        }
    }

    /// A full retry backlog must never discard recovery evidence: every local
    /// mutation VSS has not acknowledged needs a durable retry intent. On
    /// `dev` a new distinct mutation at cap evicts an arbitrary queued entry,
    /// so that entry's key silently stops replicating.
    #[test]
    fn pending_queue_cap_must_not_discard_recovery_evidence() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(&dir)));

        // Backlog accumulated by a previous run during a VSS outage, in the
        // persisted pending-row format (0x01 prefix = queued write).
        for i in 0..PENDING_QUEUE_CAP {
            let key = vss_key("", "", &format!("backlog_{i}"));
            local
                .write("", "", &format!("backlog_{i}"), vec![0xAA])
                .expect("value");
            local
                .write(PENDING_NS, "", &key, vec![1, 0xAA])
                .expect("pending row");
        }

        let synced = SyncedKvStore::with_vss(Arc::clone(&local), unreachable_vss());
        assert_eq!(synced.pending_remote_writes(), PENDING_QUEUE_CAP);

        let result = synced.write("", "", "new_mutation", b"fresh".to_vec());

        let durable_intents = local.list(PENDING_NS, "").expect("list pending").len();
        match result {
            Ok(()) => {
                assert_eq!(
                    durable_intents,
                    PENDING_QUEUE_CAP + 1,
                    "the mutation was accepted at cap, so recovery evidence \
                     was evicted: one key will never replicate to VSS"
                );
            }
            Err(_) => {
                // Fail-closed alternative: the mutation is rejected whole.
                assert!(
                    local.read("", "", "new_mutation").is_err(),
                    "a rejected mutation must not commit locally"
                );
                assert_eq!(
                    durable_intents, PENDING_QUEUE_CAP,
                    "backlog must stay intact"
                );
            }
        }
    }

    /// When `stop()` returns the caller releases the VSS single-writer fence,
    /// so no remote mutation may still be in flight. On `dev` `stop()` only
    /// waits out drains: a direct write blocked inside its VSS put is ignored
    /// and can land after another instance owns the store.
    #[test]
    fn stop_must_wait_for_inflight_remote_mutation() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(&dir)));
        let stall = StallingVss::start();
        let (signing_key, store_id) = generate_test_keys();
        let remote =
            Arc::new(VssKvStore::new(stall.url(), store_id, signing_key).expect("vss store"));
        let synced = Arc::new(SyncedKvStore::with_vss(local, remote));

        let writer = {
            let synced = Arc::clone(&synced);
            std::thread::spawn(move || synced.write("", "", "inflight", b"v1".to_vec()))
        };
        stall.wait_for_request();

        let stopper = {
            let synced = Arc::clone(&synced);
            std::thread::spawn(move || synced.stop())
        };

        // Correct behavior: stop() blocks until the in-flight put resolves,
        // so this join can only complete after the connection is cut below.
        std::thread::sleep(Duration::from_secs(2));
        let stop_returned_early = stopper.is_finished();

        stall.cut();
        stopper.join().expect("stopper thread");
        let _ = writer.join().expect("writer thread");

        assert!(
            !stop_returned_early,
            "stop() returned while a remote mutation was in flight: the VSS \
             fence can be released (and re-acquired by another instance) \
             before this put lands, breaking single-writer"
        );
    }
}
