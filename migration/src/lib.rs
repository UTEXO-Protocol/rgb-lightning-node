pub use sea_orm_migration::prelude::*;

mod m20260119_080116_create_mnemonics_table;
mod m20260119_120035_create_channel_peer_data_table;
mod m20260121_120000_create_rgb_config_table;
mod m20260126_120000_create_revoked_token_table;
mod m20260126_130000_create_channel_ids_table;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260119_080116_create_mnemonics_table::Migration),
            Box::new(m20260119_120035_create_channel_peer_data_table::Migration),
            Box::new(m20260121_120000_create_rgb_config_table::Migration),
            Box::new(m20260126_120000_create_revoked_token_table::Migration),
            Box::new(m20260126_130000_create_channel_ids_table::Migration),
        ]
    }
}
