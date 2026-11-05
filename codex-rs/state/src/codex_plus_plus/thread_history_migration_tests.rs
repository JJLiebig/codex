use super::THREAD_HISTORY_MIGRATOR;
use super::runtime_migrator_for_pool;
use pretty_assertions::assert_eq;
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::test]
async fn new_history_migrations_use_platform_checksums() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let current = runtime_migrator_for_pool(&pool, &THREAD_HISTORY_MIGRATOR)
        .await
        .unwrap();
    current.run(&pool).await.unwrap();
    for migration in current.iter() {
        assert_eq!(migration.sql.as_str().contains('\r'), cfg!(windows));
        let checksum: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
                .bind(migration.version)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(checksum, migration.checksum.as_ref());
    }
    pool.close().await;
}
