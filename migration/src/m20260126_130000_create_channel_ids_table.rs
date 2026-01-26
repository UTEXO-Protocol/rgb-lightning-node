use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ChannelIds::Table)
                    .if_not_exists()
                    .col(pk_auto(ChannelIds::Id))
                    .col(string(ChannelIds::TemporaryChannelId).unique_key())
                    .col(string(ChannelIds::ChannelId))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ChannelIds::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ChannelIds {
    Table,
    Id,
    TemporaryChannelId,
    ChannelId,
}
