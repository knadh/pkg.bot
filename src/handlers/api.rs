use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    Json,
};
use axum_extra::extract::Query;
use serde::{Deserialize, Serialize};

use super::{paginate, respond, ApiErr, ApiFormat, Ctx, Result};
use crate::{
    manager::Error,
    models::{Cursor, Package, PackageQuery, PackageResults, Repo, RepoQuery, Sort},
};

/// Maximum suggestions returned per query.
const LIMIT: usize = 15;

/// `?q=` on a suggest endpoint.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SuggestQuery {
    pub q: String,
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub num_repos: usize,
}

/// Health check endpoint for container orchestration.
pub async fn health_check(State(ctx): State<Arc<Ctx>>) -> Result<Json<HealthResponse>> {
    // Check database connectivity
    ctx.mgr.health_check().await?;

    let num_repos = ctx.repos.len();

    // If no repos loaded, return 503
    if num_repos == 0 {
        return Err(ApiErr::new("no repositories loaded", StatusCode::SERVICE_UNAVAILABLE));
    }

    Ok(Json(HealthResponse { num_repos }))
}

/// Get the list of all repositories.
pub async fn get_repos(
    State(ctx): State<Arc<Ctx>>,
    Query(filter): Query<RepoQuery>,
    format: ApiFormat,
) -> Result<Response> {
    let repos = ctx.mgr.get_repos(&Sort::asc("name"), &filter).await?;
    respond(format, repos)
}

/// Get a repository by its slug.
pub async fn get_repo(
    State(ctx): State<Arc<Ctx>>,
    Path(repo_slug): Path<String>,
    format: ApiFormat,
) -> Result<Response> {
    respond(format, get_repo_by_slug(&ctx, &repo_slug).await?)
}

/// Search packages in a repository.
pub async fn query_packages(
    State(ctx): State<Arc<Ctx>>,
    Path(repo_slug): Path<String>,
    Query(mut q): Query<PackageQuery>,
    format: ApiFormat,
) -> Result<Response> {
    let repo = get_repo_by_slug(&ctx, &repo_slug).await?;
    q.validate()
        .map_err(|e| ApiErr::new(e, StatusCode::BAD_REQUEST))?;

    // API queries only expose the first batch and don't offer pagination.
    let (page, limit, offset) = paginate(
        1,
        q.per_page,
        ctx.consts.api_max_per_page,
        ctx.consts.api_default_per_page,
    );
    q.repo_id = repo.id;
    q.page = page;
    q.per_page = limit;
    q.limit = limit;
    q.offset = offset;
    q.after.clear();
    q.before.clear();

    let (packages, _) = if q.search().0.is_empty() {
        ctx.mgr.get_packages(&q).await?
    } else {
        ctx.mgr.search_packages(&q).await?
    };

    respond(format, packages)
}

/// Get a package.
pub async fn get_package(
    State(ctx): State<Arc<Ctx>>,
    Path((repo_slug, pkg_slug)): Path<(String, String)>,
    format: ApiFormat,
) -> Result<Response> {
    let (_, package) = get_package_by_slug(&ctx, &repo_slug, &pkg_slug).await?;
    respond(format, package)
}

/// Suggest license values.
pub async fn suggest_licenses(
    State(ctx): State<Arc<Ctx>>,
    Query(q): Query<SuggestQuery>,
    format: ApiFormat,
) -> Result<Response> {
    respond(format, ctx.licenses.query(&q.q, LIMIT))
}

/// Suggest platform values.
pub async fn suggest_platforms(
    State(ctx): State<Arc<Ctx>>,
    Query(q): Query<SuggestQuery>,
    format: ApiFormat,
) -> Result<Response> {
    respond(format, ctx.platforms.query(&q.q, LIMIT))
}

/// Fetch a page of packages.
pub async fn list_packages(ctx: &Ctx, repo: &Repo, q: &PackageQuery) -> Result<PackageResults> {
    if !q.search().0.is_empty() {
        let (packages, has_more) = ctx.mgr.search_packages(q).await?;

        return Ok(PackageResults {
            packages,
            per_page: q.limit,
            total: None,
            total_capped: false,
            page: q.page,
            has_more,
            next: None,
            prev: None,
        });
    }

    let (packages, has_more) = ctx.mgr.get_packages(q).await?;
    let (total, total_capped) = if q.has_filters() {
        ctx.mgr.count_packages(q).await?
    } else {
        (repo.package_count, false)
    };

    // Keyset pagination.
    let (first, last) = (
        packages.first().map(Cursor::of),
        packages.last().map(Cursor::of),
    );
    let (next, prev) = if q.before.is_empty() {
        (
            if has_more { last } else { None },
            if q.after.is_empty() { None } else { first },
        )
    } else {
        (last, if has_more { first } else { None })
    };

    Ok(PackageResults {
        packages,
        per_page: q.limit,
        total: Some(total),
        total_capped,
        page: 0,
        has_more,
        next,
        prev,
    })
}

/// Get a repo by its slug.
async fn get_repo_by_slug(ctx: &Ctx, slug: &str) -> Result<Repo> {
    match ctx.mgr.get_repo(None, Some(slug)).await {
        Ok(repo) => Ok(repo),
        Err(Error::NotFound) => Err(ApiErr::new("Unknown repository.", StatusCode::NOT_FOUND)),
        Err(e) => Err(e.into()),
    }
}

/// Get a package by its repo + its own slug.
pub async fn get_package_by_slug(
    ctx: &Ctx,
    repo_slug: &str,
    pkg_slug: &str,
) -> Result<(Repo, Package)> {
    let repo = get_repo_by_slug(ctx, repo_slug).await?;
    let package = match ctx.mgr.get_package(repo.id, pkg_slug).await {
        Ok(package) => package,
        Err(Error::NotFound) => {
            return Err(ApiErr::new(
                "The package does not exist in this repository.",
                StatusCode::NOT_FOUND,
            ))
        }
        Err(e) => return Err(e.into()),
    };

    Ok((repo, package))
}

/// Search packages with filters.
pub async fn search_packages(
    ctx: &Ctx,
    slug: &str,
    q: &mut PackageQuery,
    max_per_page: i32,
    default_per_page: i32,
) -> Result<(Repo, PackageResults)> {
    let repo = get_repo_by_slug(ctx, slug).await?;

    q.validate()
        .map_err(|e| ApiErr::new(e, StatusCode::BAD_REQUEST))?;
    let (page, per_page, offset) = paginate(q.page, q.per_page, max_per_page, default_per_page);
    q.repo_id = repo.id;
    q.page = page;
    q.per_page = per_page;
    q.offset = offset;
    q.limit = per_page;

    let results = list_packages(ctx, &repo, q).await?;
    Ok((repo, results))
}

#[cfg(test)]
mod health_check_tests {
    use super::*;
    use crate::{
        manager::Manager,
        models::{JsonString, Repo, SCHEMA, Suggestions},
        Consts,
    };

    /// Build an app context backed by an in-memory SQLite database with the
    /// full schema installed, so the manager can run real queries against it.
    async fn make_ctx(repos: Vec<Repo>) -> Arc<Ctx> {
        let db = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("connect in-memory db");
        sqlx::query(&SCHEMA.schema.query).execute(&db).await.unwrap();
        let mgr = Arc::new(Manager::new(db));

        Arc::new(Ctx {
            mgr,
            repos,
            last_updated: None,
            licenses: Suggestions::new(vec![]),
            platforms: Suggestions::new(vec![]),
            site: None,
            consts: Consts {
                root_url: "/".to_string(),
                api_default_per_page: 20,
                api_max_per_page: 100,
                site_default_per_page: 20,
                site_max_per_page: 100,
            },
            asset_ver: "test".to_string(),
        })
    }

    fn repo(slug: &str) -> Repo {
        Repo {
            id: 1,
            slug: slug.to_string(),
            name: slug.to_string(),
            family: "test".to_string(),
            manager: "apt".to_string(),
            distro: None,
            branch: None,
            homepage_url: None,
            links: JsonString(serde_json::json!([]).to_string()),
            pkg_url_template: None,
            source_url_template: None,
            meta: JsonString(serde_json::json!({}).to_string()),
            brand_color: None,
            score: 0.0,
            package_count: 0,
            num_packages: 0,
            num_maintainers: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[tokio::test]
    async fn health_check_returns_503_when_no_repos() {
        let ctx = make_ctx(vec![]).await;
        let err = health_check(State(ctx))
            .await
            .expect_err("expected an error");
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn health_check_returns_200_with_num_repos() {
        let ctx = make_ctx(vec![repo("arch"), repo("debian")]).await;
        let Json(HealthResponse { num_repos }) =
            health_check(State(ctx)).await.expect("expected success");
        assert_eq!(num_repos, 2);
    }
}
