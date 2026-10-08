use super::THREAD_HISTORY_MIGRATOR;
use super::runtime_migrator_for_pool;
use pretty_assertions::assert_eq;
use sqlx::sqlite::SqlitePoolOptions;
use std::borrow::Cow;

#[tokio::test]
async fn released_attribution_history_upgrades_without_losing_data() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory history database");
    let mut released = super::super::runtime_thread_history_migrator();
    released.migrations = Cow::Owned(
        released
            .migrations
            .iter()
            .filter(|migration| migration.version <= 8)
            .cloned()
            .collect(),
    );
    released.run(&pool).await.expect("released fork migrations");
    sqlx::query("INSERT INTO thread_turns (thread_id, turn_id, rollout_ordinal, status, inference_attribution_json) VALUES ('thread', 'turn', 1, 'completed', '{\"type\":\"claude\"}')")
        .execute(&pool)
        .await
        .expect("released attributed turn");
    let current = runtime_migrator_for_pool(&pool, &THREAD_HISTORY_MIGRATOR)
        .await
        .expect("current migration checksums");
    current.run(&pool).await.expect("upgrade released history");
    current.run(&pool).await.expect("reopen upgraded history");
    let turn = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT inference_attribution_json, root_turn_id FROM thread_turns WHERE turn_id = 'turn'",
    )
    .fetch_one(&pool)
    .await
    .expect("preserved turn and new root column");
    assert_eq!(turn, ("{\"type\":\"claude\"}".to_string(), None));
    pool.close().await;
}
