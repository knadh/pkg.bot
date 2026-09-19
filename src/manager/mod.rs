use sqlx::sqlite::SqlitePool;

use crate::models::{
    comparisons, get_comparator, normalize_version, Cursor, Listing, Package, PackageQuery, Repo,
    RepoQuery, Sort, BY_FACET, BY_NAME, Q, SEARCH_PACKAGES,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("not found")]
    NotFound,
}

pub struct Manager {
    db: SqlitePool,
}

impl Manager {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn health_check(&self) -> Result<(), Error> {
        sqlx::query("SELECT 1").execute(&self.db).await?;
        Ok(())
    }

    /// Get a repo by its ID or slug.
    pub async fn get_repo(&self, id: Option<i64>, slug: Option<&str>) -> Result<Repo, Error> {
        sqlx::query_as(&Q.get_repo.query)
            .bind(id)
            .bind(slug)
            .fetch_optional(&self.db)
            .await?
            .ok_or(Error::NotFound)
    }

    pub async fn get_repos(&self, sort: &Sort, filter: &RepoQuery) -> Result<Vec<Repo>, Error> {
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
            .fetch_all(&self.db)
            .await?)
    }

    /// Get the default grouped repo list (sorted desc by package count).
    pub async fn get_repos_grouped(&self, filter: &RepoQuery) -> Result<Vec<Repo>, Error> {
        Ok(sqlx::query_as(&Q.get_repos_grouped.query)
            .bind(filter.family.trim())
            .bind(filter.distro.trim())
            .bind(filter.manager.trim())
            .fetch_all(&self.db)
            .await?)
    }

    pub async fn get_package(&self, repo_id: i64, slug: &str) -> Result<Package, Error> {
        sqlx::query_as(&Q.get_package.query)
            .bind(repo_id)
            .bind(slug)
            .fetch_optional(&self.db)
            .await?
            .ok_or(Error::NotFound)
    }

    /// Get alphabetical listing of a repo's packages.
    pub async fn get_packages(&self, pq: &PackageQuery) -> Result<(Vec<Package>, bool), Error> {
        let back = !pq.before.is_empty();
        let cur = Cursor::parse(if back { &pq.before } else { &pq.after });

        let Some(plan) = self.plan(pq).await? else {
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
            .fetch_all(&self.db)
            .await?;

        let has_more = packages.len() > pq.limit as usize;
        packages.truncate(pq.limit as usize);
        if back {
            packages.reverse();
        }

        Ok((packages, has_more))
    }

    /// Use the saved count for a single filter; otherwise count up to [`MAX_COUNT`].
    /// The bool is true when there are more matches than the limit.
    pub async fn count_packages(&self, pq: &PackageQuery) -> Result<(i64, bool), Error> {
        let Some(plan) = self.plan(pq).await? else {
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
            .fetch_optional(&self.db)
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
            .fetch_one(&self.db)
            .await?;

        Ok((n.min(MAX_COUNT), n > MAX_COUNT))
    }

    /// Start with the filter matching the fewest packages, then check the others.
    /// Return `None` if any filter has no matches in this repository.
    async fn plan(&self, pq: &PackageQuery) -> Result<Option<Plan>, Error> {
        let wanted = pq.facets();
        if wanted.is_empty() {
            return Ok(Some(Plan::default()));
        }

        let drive: Option<(String, String, i64)> = sqlx::query_as(&Q.pick_facet.query)
            .bind(pq.repo_id)
            .bind(to_facet_json(&wanted))
            .fetch_optional(&self.db)
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
        Ok(sqlx::query_scalar(&Q.get_licenses.query)
            .fetch_all(&self.db)
            .await?)
    }

    /// Get every platforms.
    pub async fn get_platforms(&self) -> Result<Vec<String>, Error> {
        Ok(sqlx::query_scalar(&Q.get_platforms.query)
            .fetch_all(&self.db)
            .await?)
    }

    pub async fn search_packages(&self, pq: &PackageQuery) -> Result<(Vec<Package>, bool), Error> {
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
            .fetch_all(&self.db)
            .await?;

        let has_more = packages.len() > pq.limit as usize;
        packages.truncate(pq.limit as usize);
        Ok((packages, has_more))
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
