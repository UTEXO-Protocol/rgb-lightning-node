use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RgbConfig::Table)
                    .if_not_exists()
                    .col(pk_auto(RgbConfig::Id))
                    .col(string(RgbConfig::Key).unique_key())
                    .col(text(RgbConfig::Value))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RgbConfig::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RgbConfig {
    Table,
    Id,
    Key,
    Value,
}
