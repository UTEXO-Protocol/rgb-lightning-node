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
    use lightning::rgb_utils::{RGB_CHANNEL_INFO_NS, RGB_CHANNEL_INFO_PENDING_NS, RGB_PRIMARY_NS};
    use lightning::util::persist::{
        KVStoreSync, CHANNEL_MANAGER_PERSISTENCE_KEY,
        CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
    };
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve VSS test port");
        let port = listener.local_addr().expect("VSS test address").port();
        drop(listener);
        let (signing_key, store_id) = generate_test_keys();
        Arc::new(
            VssKvStore::new(
                format!("http://127.0.0.1:{port}/vss"),
                store_id,
                signing_key,
            )
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

        fn try_request(&self) -> Result<bool, mpsc::TryRecvError> {
            match self.arrived.try_recv() {
                Ok(()) => Ok(true),
                Err(mpsc::TryRecvError::Empty) => Ok(false),
                Err(error) => Err(error),
            }
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    /// so no remote mutation may still be in flight. Event-ordered: a correct
    /// `stop()` blocks on the drain gate the in-flight write holds and can
    /// only return after the connection is cut; a `stop()` that ignores the
    /// in-flight put acquires the free gate and returns inside the
    /// observation window.
    #[serial_test::serial]
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

        let (at_gate_tx, at_gate_rx) = mpsc::channel();
        synced.set_before_stop_gate_hook(Arc::new(move || {
            let _ = at_gate_tx.send(());
        }));
        let (stop_done_tx, stop_done_rx) = mpsc::channel();
        let stopper = {
            let synced = Arc::clone(&synced);
            std::thread::spawn(move || {
                synced.stop();
                let _ = stop_done_tx.send(());
            })
        };

        at_gate_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("stop must reach the gate");
        // A stop() blocked on the held gate cannot complete until the cut
        // below, so this window can only ever observe a stop() that skipped
        // the in-flight put; the wait it bounds is an uncontended lock plus a
        // channel send.
        let stopped_early = stop_done_rx.recv_timeout(Duration::from_secs(10)).is_ok();

        stall.cut();
        stopper.join().expect("stopper thread");
        let _ = writer.join().expect("writer thread");

        assert!(
            !stopped_early,
            "stop() returned while a remote mutation was in flight: the VSS \
             fence can be released (and re-acquired by another instance) \
             before this put lands, breaking single-writer"
        );
    }

    /// A retry drain that passed its initial admission check before shutdown
    /// must not start a remote mutation after `stop()` has returned.
    #[serial_test::serial]
    #[test]
    fn queued_drain_must_not_run_after_stop() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(&dir)));
        let pending_key = vss_key("", "", "queued");
        local
            .write("", "", "queued", b"v1".to_vec())
            .expect("value");
        local
            .write(PENDING_NS, "", &pending_key, vec![1, b'v', b'1'])
            .expect("pending row");

        let stall = StallingVss::start();
        let (signing_key, store_id) = generate_test_keys();
        let remote =
            Arc::new(VssKvStore::new(stall.url(), store_id, signing_key).expect("vss store"));
        let synced = Arc::new(SyncedKvStore::with_vss(local, remote));

        let (at_gate_tx, at_gate_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = std::sync::Mutex::new(resume_rx);
        synced.set_before_drain_gate_hook(Arc::new(move || {
            at_gate_tx.send(()).expect("signal drain admission");
            resume_rx.lock().unwrap().recv().expect("resume drain");
        }));

        let (drain_done_tx, drain_done_rx) = mpsc::channel();
        let drainer = {
            let synced = Arc::clone(&synced);
            std::thread::spawn(move || {
                synced.drain_pending();
                drain_done_tx.send(()).expect("signal drain completion");
            })
        };
        at_gate_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("drain must reach the shutdown gate");

        synced.stop();
        resume_tx.send(()).expect("resume queued drain");

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let attempted_remote_write = loop {
            if stall
                .try_request()
                .expect("VSS listener must stay available")
            {
                break true;
            }
            match drain_done_rx.try_recv() {
                Ok(()) => break false,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("drain thread exited without reporting completion")
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "queued drain produced neither completion nor remote I/O"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        stall.cut();
        drainer.join().expect("drainer thread");

        assert!(
            !attempted_remote_write,
            "a queued retry drain started a remote mutation after stop() returned"
        );
        assert_eq!(synced.pending_remote_writes(), 1);
    }

    /// The funding state machine handles `psbt` and `pending_funding` write
    /// errors, so neither record may be acknowledged without remote durability.
    #[serial_test::serial]
    #[test]
    fn psbt_and_pending_funding_fail_closed_during_outage() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(&dir)));
        let synced = SyncedKvStore::with_vss(local, unreachable_vss());

        synced
            .write("psbt", "", "funding_txid", b"psbt".to_vec())
            .expect_err("psbt write must fail closed during an outage");
        synced
            .write("pending_funding", "", "channel_id", b"txid".to_vec())
            .expect_err("pending_funding write must fail closed during an outage");
        assert_eq!(synced.pending_remote_writes(), 2);
        assert_eq!(synced.read("psbt", "", "funding_txid").unwrap(), b"psbt");
        assert_eq!(
            synced.read("pending_funding", "", "channel_id").unwrap(),
            b"txid"
        );
    }

    /// A protocol transition may explicitly require remote acknowledgement without making every
    /// writer in the namespace fail closed. This keeps recovery writes strict while the broader
    /// RGB channel-info settlement policy remains a separate change.
    #[serial_test::serial]
    #[test]
    fn explicit_remote_required_write_fails_closed_during_outage() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(&dir)));
        let synced = SyncedKvStore::with_vss(local, unreachable_vss());

        synced
            .write_remote_required("protocol", "recovery", "operation_id", b"metadata".to_vec())
            .expect_err("protocol state must not advance without remote acknowledgement");

        assert_eq!(synced.pending_remote_writes(), 1);
        assert_eq!(
            synced.read("protocol", "recovery", "operation_id").unwrap(),
            b"metadata"
        );
    }

    /// An acknowledged canonical or pending RGB channel balance must survive loss of the local
    /// device. Because the legacy LDK persistence interface cannot return a typed retryable error,
    /// the writer waits for VSS recovery and must never return success while the only current value
    /// exists in the local retry queue.
    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acknowledged_rgb_channel_info_survives_device_loss() {
        if std::net::TcpStream::connect_timeout(
            &"127.0.0.1:8081".parse().unwrap(),
            Duration::from_secs(2),
        )
        .is_err()
        {
            eprintln!("SKIP: VSS server not available at http://127.0.0.1:8081/vss");
            return;
        }

        for secondary_namespace in [RGB_CHANNEL_INFO_NS, RGB_CHANNEL_INFO_PENDING_NS] {
            let proxy = super::super::vss_offline_force_close::VssProxy::start();
            let (signing_key, store_id) = generate_test_keys();
            let local_dir = tempfile::tempdir().expect("local tempdir");
            let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(
                local_dir.path(),
            )));
            let remote = Arc::new(
                VssKvStore::new(proxy.url(), store_id.clone(), signing_key).expect("vss store"),
            );
            let synced = Arc::new(SyncedKvStore::with_vss(local, remote));

            synced
                .write(
                    CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
                    CHANNEL_MANAGER_PERSISTENCE_KEY,
                    vec![0xCA; 64],
                )
                .expect("baseline manager write");
            assert_eq!(synced.pending_remote_writes(), 0);

            proxy.go_offline();
            let (result_tx, result_rx) = mpsc::sync_channel(1);
            let writer = {
                let synced = Arc::clone(&synced);
                std::thread::spawn(move || {
                    let result = synced.write(
                        RGB_PRIMARY_NS,
                        secondary_namespace,
                        "channel_id",
                        b"rgb_info".to_vec(),
                    );
                    result_tx.send(result).expect("send writer result");
                })
            };

            assert!(matches!(
                result_rx.recv_timeout(Duration::from_secs(2)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            proxy.go_online();
            result_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("critical RGB metadata write did not resume after VSS recovery")
                .expect("critical RGB metadata write failed after VSS recovery");
            writer.join().expect("writer thread");
            drop(synced);

            let restored_dir = tempfile::tempdir().expect("restored tempdir");
            let restored_local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(
                restored_dir.path(),
            )));
            let restored_remote = Arc::new(
                VssKvStore::new(
                    "http://127.0.0.1:8081/vss".to_owned(),
                    store_id,
                    signing_key,
                )
                .expect("restored vss store"),
            );
            let restored = SyncedKvStore::with_vss(restored_local, restored_remote);
            restored.restore_from_vss(true).expect("restore from VSS");

            assert_eq!(
                restored
                    .read(
                        CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
                        CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
                        CHANNEL_MANAGER_PERSISTENCE_KEY,
                    )
                    .expect("manager must be restored"),
                vec![0xCA; 64]
            );
            assert_eq!(
                restored
                    .read(RGB_PRIMARY_NS, secondary_namespace, "channel_id")
                    .expect("acknowledged RGB metadata must survive device loss"),
                b"rgb_info"
            );
        }
    }

    /// An acknowledged channel-metadata removal must not be undone by a device-loss restore.
    /// Otherwise a closed channel can reappear with a stale RGB balance split after recovery.
    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acknowledged_rgb_channel_info_removal_survives_device_loss() {
        if std::net::TcpStream::connect_timeout(
            &"127.0.0.1:8081".parse().unwrap(),
            Duration::from_secs(2),
        )
        .is_err()
        {
            eprintln!("SKIP: VSS server not available at http://127.0.0.1:8081/vss");
            return;
        }

        for secondary_namespace in [RGB_CHANNEL_INFO_NS, RGB_CHANNEL_INFO_PENDING_NS] {
            let proxy = super::super::vss_offline_force_close::VssProxy::start();
            let (signing_key, store_id) = generate_test_keys();
            let local_dir = tempfile::tempdir().expect("local tempdir");
            let local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(
                local_dir.path(),
            )));
            let remote = Arc::new(
                VssKvStore::new(proxy.url(), store_id.clone(), signing_key).expect("vss store"),
            );
            let synced = Arc::new(SyncedKvStore::with_vss(local, remote));

            synced
                .write(
                    RGB_PRIMARY_NS,
                    secondary_namespace,
                    "channel_id",
                    b"rgb_info".to_vec(),
                )
                .expect("seed RGB metadata");
            assert_eq!(synced.pending_remote_writes(), 0);

            proxy.go_offline();
            let (result_tx, result_rx) = mpsc::sync_channel(1);
            let remover = {
                let synced = Arc::clone(&synced);
                std::thread::spawn(move || {
                    let result =
                        synced.remove(RGB_PRIMARY_NS, secondary_namespace, "channel_id", false);
                    result_tx.send(result).expect("send remover result");
                })
            };

            assert!(matches!(
                result_rx.recv_timeout(Duration::from_secs(2)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            proxy.go_online();
            result_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("critical RGB metadata removal did not resume after VSS recovery")
                .expect("critical RGB metadata removal failed after VSS recovery");
            remover.join().expect("remover thread");
            drop(synced);

            let restored_dir = tempfile::tempdir().expect("restored tempdir");
            let restored_local = Arc::new(SeaOrmKvStore::from_connection(open_sqlite(
                restored_dir.path(),
            )));
            let restored_remote = Arc::new(
                VssKvStore::new(
                    "http://127.0.0.1:8081/vss".to_owned(),
                    store_id,
                    signing_key,
                )
                .expect("restored vss store"),
            );
            let restored = SyncedKvStore::with_vss(restored_local, restored_remote);
            restored.restore_from_vss(true).expect("restore from VSS");

            assert!(matches!(
                restored.read(RGB_PRIMARY_NS, secondary_namespace, "channel_id"),
                Err(error) if error.kind() == bitcoin::io::ErrorKind::NotFound
            ));
        }
    }
}
