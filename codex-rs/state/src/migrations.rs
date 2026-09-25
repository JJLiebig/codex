use std::borrow::Cow;
use std::collections::HashMap;

use sqlx::AssertSqlSafe;
use sqlx::SqlSafeStr;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;
use sqlx_sqlite::SqlitePool;

pub(crate) static STATE_MIGRATOR: Migrator = sqlx_macros::migrate!("./migrations");
pub(crate) static LOGS_MIGRATOR: Migrator = sqlx_macros::migrate!("./logs_migrations");
pub(crate) static GOALS_MIGRATOR: Migrator = sqlx_macros::migrate!("./goals_migrations");
pub(crate) static MEMORIES_MIGRATOR: Migrator = sqlx_macros::migrate!("./memory_migrations");
pub(crate) static QUEUE_MIGRATOR: Migrator = sqlx_macros::migrate!("./queue_migrations");
pub(crate) static THREAD_HISTORY_MIGRATOR: Migrator =
    sqlx_macros::migrate!("./thread_history_migrations");

#[derive(Clone, Copy)]
enum MigrationLineEndings {
    Lf,
    Crlf,
}

/// Allow an older Codex binary to open a database that has already been
/// migrated by a newer binary running in parallel.
///
/// We intentionally ignore applied migration versions that are newer than the
/// embedded migration set. Known migration versions are still validated by
/// checksum, so this only relaxes the "database is ahead of me" case.
fn runtime_migrator(base: &'static Migrator) -> Migrator {
    Migrator {
        migrations: Cow::Borrowed(base.migrations.as_ref()),
        ignore_missing: true,
        locking: base.locking,
        no_tx: base.no_tx,
        table_name: base.table_name.clone(),
        create_schemas: base.create_schemas.clone(),
    }
}

pub(crate) fn runtime_state_migrator() -> Migrator {
    runtime_migrator(&STATE_MIGRATOR)
}

pub(crate) fn runtime_logs_migrator() -> Migrator {
    runtime_migrator(&LOGS_MIGRATOR)
}

pub(crate) fn runtime_goals_migrator() -> Migrator {
    runtime_migrator(&GOALS_MIGRATOR)
}

pub(crate) fn runtime_memories_migrator() -> Migrator {
    runtime_migrator(&MEMORIES_MIGRATOR)
}

pub(crate) fn runtime_queue_migrator() -> Migrator {
    runtime_migrator(&QUEUE_MIGRATOR)
}

// The paginated history projector will call this when it takes ownership of opening the database.
#[allow(dead_code)]
pub(crate) fn runtime_thread_history_migrator() -> Migrator {
    runtime_migrator(&THREAD_HISTORY_MIGRATOR)
}

pub(crate) async fn runtime_migrator_for_pool(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<Migrator> {
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    let applied = if migrations_table_exists {
        sqlx::query_as::<_, (i64, Vec<u8>)>("SELECT version, checksum FROM _sqlx_migrations")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };
    let migrations = migrator
        .migrations
        .iter()
        .map(|migration| {
            let lf_migration = migration_with_line_endings(migration, MigrationLineEndings::Lf);
            let crlf_migration = migration_with_line_endings(migration, MigrationLineEndings::Crlf);
            let applied_checksum = applied.get(&migration.version).or_else(|| {
                (migration.version == 39)
                    .then(|| applied.get(&38))
                    .flatten()
                    .filter(|checksum| {
                        checksum.as_slice() == lf_migration.checksum.as_ref()
                            || checksum.as_slice() == crlf_migration.checksum.as_ref()
                    })
            });
            let Some(applied_checksum) = applied_checksum else {
                return if cfg!(windows) {
                    crlf_migration
                } else {
                    lf_migration
                };
            };
            if applied_checksum == migration.checksum.as_ref() {
                return migration.clone();
            }
            if applied_checksum == lf_migration.checksum.as_ref() {
                return lf_migration;
            }
            if applied_checksum == crlf_migration.checksum.as_ref() {
                crlf_migration
            } else {
                migration.clone()
            }
        })
        .collect();
    Ok(Migrator {
        migrations: Cow::Owned(migrations),
        ignore_missing: migrator.ignore_missing,
        locking: migrator.locking,
        no_tx: migrator.no_tx,
        table_name: migrator.table_name.clone(),
        create_schemas: migrator.create_schemas.clone(),
    })
}

fn migration_with_line_endings(
    migration: &Migration,
    line_endings: MigrationLineEndings,
) -> Migration {
    let lf_sql = migration
        .sql
        .as_str()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let sql = match line_endings {
        MigrationLineEndings::Lf => lf_sql,
        MigrationLineEndings::Crlf => lf_sql.replace('\n', "\r\n"),
    };
    if migration.sql == sql {
        return migration.clone();
    }
    Migration::new(
        migration.version,
        migration.description.clone(),
        migration.migration_type,
        AssertSqlSafe(sql).into_sql_str(),
        migration.no_tx,
    )
}

pub(crate) async fn repair_legacy_recency_migration_version(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<()> {
    let Some(recency_migration) = migrator
        .migrations
        .iter()
        .find(|migration| migration.version == 39)
    else {
        return Ok(());
    };
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if !migrations_table_exists {
        return Ok(());
    }

    let legacy_recency_needs_repair = sqlx::query_scalar::<_, i64>(
        r#"
SELECT 1
FROM _sqlx_migrations
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
        "#,
    )
    .bind(38_i64)
    .bind(recency_migration.checksum.as_ref())
    .bind(recency_migration.version)
    .fetch_optional(pool)
    .await?
    .is_some();
    if !legacy_recency_needs_repair {
        return Ok(());
    }

    sqlx::query(
        r#"
UPDATE _sqlx_migrations
SET version = ?, description = ?
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
        "#,
    )
    .bind(recency_migration.version)
    .bind(recency_migration.description.as_ref())
    .bind(38_i64)
    .bind(recency_migration.checksum.as_ref())
    .bind(recency_migration.version)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
