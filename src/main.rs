mod cli;
mod config;
mod db;
mod feed;
mod handlers;
mod http;
mod importer;
mod init;
mod manager;
mod models;
mod repology;

// Use mimalloc for musl builds (musl's default malloc is very slow).
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::sync::Arc;

use clap::Parser;

use cli::Commands;
use handlers::{Consts, Ctx, Site};
use manager::Manager;
use models::{RepoQuery, Sort, Suggestions};

#[tokio::main]
async fn main() {
    init::logger();

    let cli = cli::Cli::parse();

    // DB path from --db flag.
    let db_path = cli.db_path.to_string_lossy().to_string();

    // Handle CLI flags.
    if let Some(cmd) = cli.command {
        match cmd {
            Commands::RestoreRepology { pg } => {
                if let Err(e) = repology::run(&pg).await {
                    log::error!("repology restore failed: {e}");
                    std::process::exit(1);
                }
                return;
            }

            // Import Repology PG dump into a new SQLite db.
            Commands::Import { pg } => {
                let config = config::load_all(&cli.config);
                if let Err(e) = importer::run(&pg, &cli.db_path, &config.import).await {
                    log::error!("import failed: {e}");
                    std::process::exit(1);
                }
                return;
            }

            // Generate a new config file.
            Commands::NewConfig { path } => {
                match config::generate_sample(&path) {
                    Ok(_) => {
                        log::info!("config file generated: {}", path.display());
                    }
                    Err(e) => {
                        log::error!("error generating config: {}", e);
                        std::process::exit(1);
                    }
                }
                return;
            }

            // Create a new SQLite database with schema.
            Commands::Install { yes } => {
                if cli.db_path.exists() {
                    log::error!("database '{}' already exists", cli.db_path.display());
                    std::process::exit(1);
                }
                if let Err(e) = db::install_schema(&db_path, !yes).await {
                    log::error!("error installing schema: {}", e);
                    std::process::exit(1);
                }
                return;
            }
        }
    }

    // For server mode, DB must exist.
    db::exists(&cli.db_path);

    // Load config.
    let config = config::load_all(&cli.config);

    // Create database pool.
    if let Err(e) = config.db.validate() {
        log::error!("invalid configuration: {e}");
        std::process::exit(1);
    }
    let db = match db::init(&db_path, config.db.max_conns, false).await {
        Ok(pool) => pool,
        Err(e) => {
            log::error!("error connecting to database: {}", e);
            std::process::exit(1);
        }
    };

    // Separate DB connection for health checks.
    let health_db = db::init(&db_path, 1, true).await.unwrap_or_else(|e| {
        log::error!("error connecting health check to database: {e}");
        std::process::exit(1);
    });

    // Initialize manager.
    let mgr = Arc::new(Manager::new(db, health_db, &config.db));

    // Load the repos once. They only change when an indexer adds one.
    let repos = match mgr
        .get_repos(&Sort::asc("name"), &RepoQuery::default())
        .await
    {
        Ok(r) => r,
        Err(e) => {
            log::error!("error loading repos: {}", e);
            std::process::exit(1);
        }
    };
    log::info!("loaded {} repos", repos.len());
    handlers::site::init_latest_repos(&repos);
    let last_updated = repos.iter().map(|r| &r.updated_at).max().cloned();

    // Autocomplete fields.
    let licenses = Suggestions::new(load(mgr.get_licenses().await, "licenses"));
    let platforms = Suggestions::new(load(mgr.get_platforms().await, "platforms"));
    log::info!(
        "loaded {} licenses, {} platforms",
        licenses.len(),
        platforms.len()
    );

    // Initialize the HTML site templates.
    let site = cli.site.map(|path| {
        let tpl = init::site_tpls(&path).unwrap_or_else(|e| {
            log::error!("error loading templates from {}: {}", path.display(), e);
            std::process::exit(1);
        });

        Site { tpl, path }
    });

    // Setup the global app context used in HTTP handlers.
    let ctx = Arc::new(Ctx {
        mgr,
        repos,
        last_updated,
        licenses,
        platforms,
        site,

        // Global constants populated from config.
        consts: Consts {
            root_url: config.app.root_url,

            api_default_per_page: config.api_results.per_page,
            api_max_per_page: config.api_results.max_per_page,

            site_default_per_page: config.site_results.per_page,
            site_max_per_page: config.site_results.max_per_page,
        },

        // Random string for busting static asset caches.
        asset_ver: format!(
            "{:08}",
            chrono::Local::now().timestamp_nanos_opt().unwrap_or(0) % 100_000_000
        ),
    });

    // Start the HTTP server.
    let routes = http::init_handlers(ctx);
    let addr = config.app.address;

    log::info!("starting server on {}", addr);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("error listening on {}: {}", addr, e);
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, routes).await {
        log::error!("server error: {}", e);
        std::process::exit(1);
    }
}

fn load<T, E: std::fmt::Display>(res: Result<T, E>, what: &str) -> T {
    res.unwrap_or_else(|e| {
        log::error!("error loading {}: {}", what, e);
        std::process::exit(1);
    })
}
