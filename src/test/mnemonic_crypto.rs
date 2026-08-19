use rln_migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};

use crate::crypto::encrypt_mnemonic;
use crate::database::RlnDatabase;
use crate::error::APIError;
use crate::utils::{check_password_validity, encrypt_and_save_mnemonic};

const PASSWORD: &str = "password123";
const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
// Record written by the magic-crypt encryption used before scrypt and XChaCha20Poly1305, as found
// in wallets and backups created by earlier versions.
const LEGACY_RECORD: &str = "m6g98F3pqJ49njHK+XWpIBUKEuxv2Gy6Qlt8S900rkc7FA4aMG3hfRAUYYEOJfdUtwqDnImV8W7Rdy6zLcY8oBFbdRGdz9kXb5iRY1BPf81gPX0OEa7B4Cn/dsNNCMJ1";

fn setup_db() -> (tempfile::TempDir, DatabaseConnection) {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let db_path = tmp_dir.path().join("rln_db");
    let connection_string = format!("sqlite:{}?mode=rwc", db_path.display());
    let db = crate::runtime::block_on(Database::connect(ConnectOptions::new(connection_string)))
        .expect("db connection");
    crate::runtime::block_on(Migrator::up(&db, None)).expect("run migrations");
    (tmp_dir, db)
}

#[test]
fn mnemonic_roundtrips_through_the_database() {
    let (_tmp_dir, db) = setup_db();
    encrypt_and_save_mnemonic(PASSWORD.to_string(), MNEMONIC.to_string(), &db).expect("save");
    let mnemonic = check_password_validity(PASSWORD, &db).expect("read back");
    assert_eq!(mnemonic.to_string(), MNEMONIC);
}

#[test]
fn wrong_password_is_reported_as_such() {
    let (_tmp_dir, db) = setup_db();
    encrypt_and_save_mnemonic(PASSWORD.to_string(), MNEMONIC.to_string(), &db).expect("save");
    assert!(matches!(
        check_password_validity("wrong-password", &db),
        Err(APIError::WrongPassword)
    ));
}

#[test]
fn legacy_record_is_reported_as_corrupted() {
    let (_tmp_dir, db) = setup_db();
    RlnDatabase::new(db.clone())
        .save_mnemonic(LEGACY_RECORD.to_string())
        .expect("save");
    assert!(matches!(
        check_password_validity(PASSWORD, &db),
        Err(APIError::CorruptedMnemonic(_))
    ));
}

#[test]
fn decryptable_but_invalid_mnemonic_is_reported_as_corrupted() {
    let (_tmp_dir, db) = setup_db();
    let encrypted = encrypt_mnemonic(PASSWORD, "not a bip39 mnemonic").expect("encrypt");
    RlnDatabase::new(db.clone())
        .save_mnemonic(encrypted)
        .expect("save");
    assert!(matches!(
        check_password_validity(PASSWORD, &db),
        Err(APIError::CorruptedMnemonic(_))
    ));
}

#[test]
fn missing_record_is_not_initialized() {
    let (_tmp_dir, db) = setup_db();
    assert!(matches!(
        check_password_validity(PASSWORD, &db),
        Err(APIError::NotInitialized)
    ));
}
