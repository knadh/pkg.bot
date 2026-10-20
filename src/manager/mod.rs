mod query;

use std::time::Duration;

use sqlx::sqlite::{SqliteConnection, SqlitePool};

use query::QueryConnection;

use crate::models::{
    comparisons, get_comparator, normalize_version, Cursor, DbConfig, Listing, Package,
    PackageQuery, Repo, RepoQuery, Sort, BY_FACET, BY_NAME, Q, SEARCH_PACKAGES,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(sqlx::Error),
    #[error("not found")]
    NotFound,
    #[error("server is busy; try after a while")]
    Busy,
    #[error("query timed out")]
    Timeout,
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::PoolTimedOut => Self::Busy,
            sqlx::Error::Database(db) if db.code().as_deref() == Some("9") => {
                log::warn!("SQLite query interrupted after reaching its deadline");
                Self::Timeout
            }
            _ => Self::Db(err),
        }
    }
}

pub struct Manager {
    db: SqlitePool,
    health_db: SqlitePool,
    query_timeout: Duration,
}

impl Manager {
    pub fn new(db: SqlitePool, health_db: SqlitePool, config: &DbConfig) -> Self {
        Self {
            db,
            health_db,
            query_timeout: Duration::from_millis(config.query_timeout_ms),
        }
    }

    /// All statements in a query share one combined execution deadline and one conn.
    async fn query<T>(
        &self,
        run: impl AsyncFnOnce(&mut SqliteConnection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut conn = QueryConnection::acquire(&self.db, self.query_timeout).await?;
        tokio::time::timeout_at(conn.deadline(), run(conn.connection()))
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// Check whether the database is reachable.
    pub async fn health_check(&self) -> Result<(), Error> {
        tokio::time::timeout(
            Duration::from_secs(1),
            sqlx::query("SELECT 1").execute(&self.health_db),
        )
        .await
        .map_err(|_| Error::Busy)??;
        Ok(())
    }

    /// Get a repo by its ID or slug.
    pub async fn get_repo(&self, id: Option<i64>, slug: Option<&str>) -> Result<Repo, Error> {
        self.query(async |conn| {
            sqlx::query_as(&Q.get_repo.query)
                .bind(id)
                .bind(slug)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or(Error::NotFound)
        })
        .await
    }

    pub async fn get_repos(&self, sort: &Sort, filter: &RepoQuery) -> Result<Vec<Repo>, Error> {
        self.query(async |conn| {
            // Optional sort.
            let sql = Q
                .get_repos
                .query
                .replace("{ORDER_BY}", &sort.order_by)
                .replace("{ORDER}", &sort.order);

            Ok(sqlx::query_as(&sql)
                .bind(filter.family.trim())
                .bind(filter.distro.trim())
                .bind(filter.manager.trim())
                .fetch_all(&mut *conn)
                .await?)
        })
        .await
    }

    /// Get the default grouped repo list (sorted desc by package count).
    pub async fn get_repos_grouped(&self, filter: &RepoQuery) -> Result<Vec<Repo>, Error> {
        self.query(async |conn| {
            Ok(sqlx::query_as(&Q.get_repos_grouped.query)
                .bind(filter.family.trim())
                .bind(filter.distro.trim())
                .bind(filter.manager.trim())
                .fetch_all(&mut *conn)
                .await?)
        })
        .await
    }

    pub async fn get_package(&self, repo_id: i64, slug: &str) -> Result<Package, Error> {
        self.query(async |conn| {
            sqlx::query_as(&Q.get_package.query)
                .bind(repo_id)
                .bind(slug)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or(Error::NotFound)
        })
        .await
    }

    /// Get alphabetical listing of a repo's packages.
    pub async fn get_packages(&self, pq: &PackageQuery) -> Result<(Vec<Package>, bool), Error> {
        self.query(async |conn| {
            let back = !pq.before.is_empty();
            let cur = Cursor::parse(if back { &pq.before } else { &pq.after });

            let Some(plan) = self.plan(pq, &mut *conn).await? else {
                return Ok((vec![], false));
            };

            // Fetch one extra row to detect whether there is another page after this.
            let sql = if back { &plan.lst.prev } else { &plan.lst.next };
            let sql = comparisons(sql, pq, 9);
            let mut packages: Vec<Package> = sqlx::query_as(&sql)
                .bind(pq.repo_id)
                .bind(&plan.kind)
                .bind(&plan.value)
                .bind(&plan.residual)
                .bind(pq.platform.trim())
                .bind(&cur.name)
                .bind(cur.id)
                .bind(pq.limit + 1)
                .bind(normalize_version(get_comparator(&pq.version).1))
                .bind((!pq.updated_at.is_empty()).then(|| get_comparator(&pq.updated_at).1))
                .fetch_all(&mut *conn)
                .await?;

            let has_more = packages.len() > pq.limit as usize;
            packages.truncate(pq.limit as usize);
            if back {
                packages.reverse();
            }

            Ok((packages, has_more))
        })
        .await
    }

    /// Use the saved count for a single filter; otherwise count up to [`MAX_COUNT`].
    /// The bool is true when there are more matches than the limit.
    pub async fn count_packages(&self, pq: &PackageQuery) -> Result<(i64, bool), Error> {
        self.query(async |conn| {
            let Some(plan) = self.plan(pq, &mut *conn).await? else {
                return Ok((0, false));
            };

            if plan.residual == "[]"
                && pq.platform.trim().is_empty()
                && !plan.kind.is_empty()
                && pq.version.is_empty()
                && pq.updated_at.is_empty()
            {
                let exact: Option<i64> = sqlx::query_scalar(
                    "SELECT package_count FROM facet_counts
                     WHERE repo_id = $1 AND kind = $2 AND value = $3",
                )
                .bind(pq.repo_id)
                .bind(&plan.kind)
                .bind(&plan.value)
                .fetch_optional(&mut *conn)
                .await?;

                if let Some(n) = exact {
                    return Ok((n, false));
                }
            }

            let sql = comparisons(&plan.lst.count, pq, 7);
            let n: i64 = sqlx::query_scalar(&sql)
                .bind(pq.repo_id)
                .bind(&plan.kind)
                .bind(&plan.value)
                .bind(&plan.residual)
                .bind(pq.platform.trim())
                .bind(MAX_COUNT + 1)
                .bind(normalize_version(get_comparator(&pq.version).1))
                .bind((!pq.updated_at.is_empty()).then(|| get_comparator(&pq.updated_at).1))
                .fetch_one(&mut *conn)
                .await?;

            Ok((n.min(MAX_COUNT), n > MAX_COUNT))
        })
        .await
    }

    /// Start with the filter matching the fewest packages, then check the others.
    /// Return `None` if any filter has no matches in this repository.
    async fn plan(
        &self,
        pq: &PackageQuery,
        conn: &mut SqliteConnection,
    ) -> Result<Option<Plan>, Error> {
        let wanted = pq.facets();
        if wanted.is_empty() {
            return Ok(Some(Plan::default()));
        }

        let drive: Option<(String, String, i64)> = sqlx::query_as(&Q.pick_facet.query)
            .bind(pq.repo_id)
            .bind(to_facet_json(&wanted))
            .fetch_optional(conn)
            .await?;

        let Some((kind, value, matched)) = drive else {
            return Ok(None);
        };
        if matched < wanted.len() as i64 {
            return Ok(None);
        }

        let rest: Vec<(&str, &str)> = wanted
            .into_iter()
            .filter(|(k, v)| *k != kind || *v != value)
            .collect();

        Ok(Some(Plan {
            lst: &BY_FACET,
            residual: to_facet_json(&rest),
            kind,
            value,
        }))
    }

    /// Get unique license values.
    pub async fn get_licenses(&self) -> Result<Vec<String>, Error> {
        self.query(async |conn| {
            Ok(sqlx::query_scalar(&Q.get_licenses.query)
                .fetch_all(&mut *conn)
                .await?)
        })
        .await
    }

    /// Get every platforms.
    pub async fn get_platforms(&self) -> Result<Vec<String>, Error> {
        self.query(async |conn| {
            Ok(sqlx::query_scalar(&Q.get_platforms.query)
                .fetch_all(&mut *conn)
                .await?)
        })
        .await
    }

    pub async fn search_packages(&self, pq: &PackageQuery) -> Result<(Vec<Package>, bool), Error> {
        self.query(async |conn| {
            let (term, name_only) = pq.search();
            let fts = to_fts_query(term, pq.repo_id, name_only);
            if fts.is_empty() {
                return Ok((vec![], false));
            }

            let raw = term.to_lowercase();
            let norm = norm_name(term);
            let wanted = pq.facets();

            let sql = comparisons(&SEARCH_PACKAGES, pq, 9);
            let mut packages: Vec<Package> = sqlx::query_as(&sql)
                .bind(pq.repo_id)
                .bind(fts)
                .bind(&raw)
                .bind(&norm)
                .bind(to_facet_json(&wanted))
                .bind(pq.platform.trim())
                .bind(pq.offset)
                .bind(pq.limit + 1)
                .bind(normalize_version(get_comparator(&pq.version).1))
                .bind((!pq.updated_at.is_empty()).then(|| get_comparator(&pq.updated_at).1))
                .fetch_all(&mut *conn)
                .await?;

            let has_more = packages.len() > pq.limit as usize;
            packages.truncate(pq.limit as usize);
            Ok((packages, has_more))
        })
        .await
    }
}

/// The table to browse and the filters to apply.
struct Plan {
    lst: &'static Listing,
    kind: String,
    value: String,
    residual: String,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            lst: &BY_NAME,
            kind: String::new(),
            value: String::new(),
            residual: "[]".into(),
        }
    }
}

/// A filtered listing counts no further than this. An exact total means testing
/// every package in the repo, which no index can avoid for the JSON filters.
pub const MAX_COUNT: i64 = 1000;

const PREFIX_MIN_LEN: usize = 3;

/// Build an FTS5 query that matches every search term in the requested fields and repo.
/// Allow prefix matches on the last term if it is long enough.
fn to_fts_query(query: &str, repo_id: i64, name_only: bool) -> String {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();

    if terms.is_empty() {
        return String::new();
    }

    let last = terms.len() - 1;
    let terms: Vec<String> = terms
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i == last && t.chars().count() >= PREFIX_MIN_LEN {
                format!("\"{t}\" *")
            } else {
                format!("\"{t}\"")
            }
        })
        .collect();

    let fields = if name_only {
        "identity"
    } else {
        "{identity keywords body}"
    };
    let tokens = format!("{fields} : ({})", terms.join(" AND "));
    if repo_id > 0 {
        format!("repo : {repo_id} AND {tokens}")
    } else {
        tokens
    }
}

/// Mirrors the transform the indexer applies to `packages.name_norm` so that
/// dash/underscore/case variants of a query match. Eg: "Foo_Bar-1" => "foobar1"
fn norm_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Encode filters for SQL, e.g. [{"k":"license","v":"MIT"}].
fn to_facet_json(pairs: &[(&str, &str)]) -> String {
    let objs: Vec<_> = pairs
        .iter()
        .map(|(k, v)| serde_json::json!({"k": k, "v": v}))
        .collect();
    serde_json::to_string(&objs).unwrap_or_else(|_| "[]".to_string())
}
