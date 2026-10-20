use std::{
    io::{BufRead, Write},
    path::Path,
    time::Duration,
};

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::Executor;

use crate::models::SCHEMA;

/// Install database schema.
pub async fn install_schema(db_path: &str, prompt: bool) -> Result<(), Box<dyn std::error::Error>> {
    if prompt {
        println!("\n** Initialize new database at '{}'? **\n", db_path);
        print!("continue (y/n)?  ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().lock().read_line(&mut input)?;
        if input.trim().to_lowercase() != "y" {
            println!("install cancelled");
            return Ok(());
        }
    }

    // Create new database.
    let db = init(db_path, 1, false).await?;

    // Install schema.
    sqlx::query(&SCHEMA.schema.query).execute(&db).await?;

    log::info!("successfully installed schema");
    Ok(())
}

/// Check if the DB file exists and exit with error message if not.
pub fn exists(path: &Path) {
    if !path.exists() {
        log::error!(
            "database '{}' not found. Run `install` to create a new one.",
            path.display()
        );
        std::process::exit(1);
    }
}

/// Create a SQLite connection pool.
pub async fn init(
    db_path: &str,
    max_conns: u32,
    read_only: bool,
) -> Result<SqlitePool, sqlx::Error> {
    let mode = if read_only { "ro" } else { "rwc" };
    let db = pool_options(max_conns)
        .after_connect(|conn, _| {
            Box::pin(async move {
                conn.execute(SCHEMA.pragma.query.as_str()).await?;
                Ok(())
            })
        })
        .connect(&format!("sqlite://{}?mode={}", db_path, mode))
        .await?;

    Ok(db)
}

pub fn pool_options(max_conns: u32) -> SqlitePoolOptions {
    SqlitePoolOptions::new()
        .max_connections(max_conns)
        .acquire_timeout(Duration::from_secs(1))
        .after_release(|conn, _| {
            Box::pin(async move {
                // Wait for cancelled SQLite work to stop.
                conn.lock_handle().await?.remove_progress_handler();
                Ok(true)
            })
        })
}
