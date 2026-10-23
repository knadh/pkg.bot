use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use super::Error;
use sqlx::{pool::PoolConnection, Sqlite, SqliteConnection, SqlitePool};

/// Dropping a SQLx future does not interrupt sqlite3_step(). Signal the
/// progress handler on cancellation; the pool clears it before reuse.
pub(super) struct QueryConnection {
    conn: PoolConnection<Sqlite>,
    cancelled: Arc<AtomicBool>,
    deadline: tokio::time::Instant,
}

impl QueryConnection {
    pub async fn acquire(db: &SqlitePool, timeout: Duration) -> Result<Self, Error> {
        let conn = db.acquire().await?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + timeout;
        let mut query = Self {
            conn,
            cancelled: cancelled.clone(),
            deadline: deadline.into(),
        };
        tokio::time::timeout_at(query.deadline, query.conn.lock_handle())
            .await
            .map_err(|_| Error::Timeout)??
            .set_progress_handler(1000, move || {
                !cancelled.load(Ordering::Relaxed) && Instant::now() < deadline
            });
        Ok(query)
    }

    pub fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    pub fn connection(&mut self) -> &mut SqliteConnection {
        &mut self.conn
    }
}

impl Drop for QueryConnection {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    const EXPENSIVE: &str = "WITH RECURSIVE n(x) AS (
        VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 1000000000
    ) SELECT sum(x) FROM n";

    async fn pool(size: u32) -> SqlitePool {
        crate::db::pool_options(size)
            .acquire_timeout(Duration::from_millis(200))
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn deadline_interrupts_sqlite_and_connection_can_be_reused() {
        let db = pool(1).await;
        let mut conn = QueryConnection::acquire(&db, Duration::from_millis(20))
            .await
            .unwrap();
        let err = timeout(
            Duration::from_secs(2),
            sqlx::query(EXPENSIVE).execute(conn.connection()),
        )
        .await
        .expect("SQLite deadline did not fire")
        .unwrap_err();
        assert!(matches!(Error::from(err), Error::Timeout));
        drop(conn);

        // Reuse beyond the old deadline, including a query with enough VM steps
        // to invoke a leaked progress handler.
        let sum: i64 = sqlx::query_scalar(
            "WITH RECURSIVE n(x) AS (
                VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000
            ) SELECT sum(x) FROM n",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(sum, 50005000);
    }

    #[tokio::test]
    async fn cancellation_stops_sqlite_before_the_execution_deadline() {
        let db = pool(1).await;
        let mut conn = QueryConnection::acquire(&db, Duration::from_secs(30))
            .await
            .unwrap();
        sqlx::query("CREATE TEMP TABLE reused (id INTEGER)")
            .execute(conn.connection())
            .await
            .unwrap();
        let job =
            tokio::spawn(async move { sqlx::query(EXPENSIVE).execute(conn.connection()).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());

        // A temporary table proves this is the same connection, rather than a
        // replacement opened while the old worker continues consuming CPU.
        sqlx::query("SELECT * FROM reused")
            .execute(&db)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_timeouts_release_pool_and_health_check_is_independent() {
        let db = pool(2).await;
        let mgr = Arc::new(super::super::Manager::new(
            db.clone(),
            pool(1).await,
            &crate::models::DbConfig {
                max_conns: 2,
                query_timeout_ms: 1000,
            },
        ));
        let ready = Arc::new(tokio::sync::Barrier::new(3));
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let mgr = mgr.clone();
            let ready = ready.clone();
            jobs.spawn(async move {
                mgr.query(async |conn| {
                    ready.wait().await;
                    sqlx::query(EXPENSIVE).execute(conn).await?;
                    Ok(())
                })
                .await
            });
        }
        timeout(Duration::from_secs(2), ready.wait()).await.unwrap();

        mgr.health_check().await.unwrap();
        assert!(matches!(
            mgr.query(async |_| Ok(())).await,
            Err(Error::Busy)
        ));
        timeout(Duration::from_secs(3), async {
            while let Some(result) = jobs.join_next().await {
                assert!(matches!(result.unwrap(), Err(Error::Timeout)));
            }
        })
        .await
        .expect("concurrent queries did not time out");

        // Hold both connections to verify that both slots were returned.
        let first = db.acquire().await.unwrap();
        let second = db.acquire().await.unwrap();
        drop((first, second));
        mgr.query(async |conn| {
            sqlx::query("SELECT 1").execute(conn).await?;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn async_waits_also_obey_the_execution_deadline() {
        let db = pool(1).await;
        let mgr = super::super::Manager::new(
            db.clone(),
            pool(1).await,
            &crate::models::DbConfig {
                max_conns: 1,
                query_timeout_ms: 20,
            },
        );
        let result = timeout(
            Duration::from_secs(2),
            mgr.query(async |conn| {
                sqlx::query("SELECT 1").execute(&mut *conn).await?;
                std::future::pending::<()>().await;
                Ok(())
            }),
        )
        .await
        .expect("async deadline did not fire");
        assert!(matches!(result, Err(Error::Timeout)));
        sqlx::query("SELECT 1").execute(&db).await.unwrap();
    }
}
