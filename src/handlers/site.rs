use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use axum::{
    extract::{Path, RawQuery, State},
    http::{header, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
    Extension,
};
use axum_extra::extract::Query;

use super::{
    api::{get_package_by_slug, search_packages},
    paginate, ApiErr, Ctx, ReqStarted,
};
use crate::feed::{Feed, Item};
use crate::models::{
    url_template, Package, PackageQuery, PackageResults, Repo, RepoQuery, Sort, REPO_SORT_FIELDS,
};

/// Latest repos (latest version per distro) that's rendered in the search bar.
static LATEST_REPOS: OnceLock<Vec<Repo>> = OnceLock::new();

pub fn init_latest_repos(repos: &[Repo]) {
    LATEST_REPOS.get_or_init(|| get_latest_repos(repos).into_iter().cloned().collect());
}

/// Landing page.
pub async fn render_index(State(ctx): State<Arc<Ctx>>) -> Response {
    let mut tpl_ctx = base_context(&ctx);
    tpl_ctx.insert("page_type", "index");
    tpl_ctx.insert("last_updated", &ctx.last_updated);
    tpl_ctx.insert(
        "total_packages",
        &ctx.repos.iter().map(|r| r.package_count).sum::<i64>(),
    );

    render(&ctx, "index.html", &mut tpl_ctx)
}

/// Render custom HTML pages from the site's pages directory.
pub async fn render_custom_page(State(ctx): State<Arc<Ctx>>, Path(page): Path<String>) -> Response {
    let template = format!("pages/{}.html", page);
    let Some(site) = &ctx.site else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if page.contains(['/', '\\']) || site.tpl.get_template(&template).is_err() {
        return not_found(&ctx, "Page does not exist.");
    }

    let mut tpl_ctx = base_context(&ctx);
    tpl_ctx.insert("page_type", "page");
    tpl_ctx.insert("page_id", &page);

    render(&ctx, &template, &mut tpl_ctx)
}

/// Repository directory.
pub async fn render_repos(
    State(ctx): State<Arc<Ctx>>,
    uri: Uri,
    Query(sort): Query<Sort>,
    Query(filter): Query<RepoQuery>,
) -> Response {
    let sort = sort.clamp(&REPO_SORT_FIELDS, "");

    // `/repos.xml` is the RSS feed for the same results.
    // This is added as a hack as axum can't register `{dynamic}.xml` routes.
    let feed = uri.path().ends_with(".xml");

    // Sort and filter.
    let repos = match if sort.order_by.is_empty() {
        ctx.mgr.get_repos_grouped(&filter).await
    } else {
        ctx.mgr.get_repos(&sort, &filter).await
    } {
        Ok(r) => r,
        Err(e) => {
            log::error!("error fetching repos: {}", e);
            return render_message(
                &ctx,
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error",
                "Error fetching the repositories.",
            );
        }
    };

    if feed {
        return render_feed(render_repos_rss(&ctx, &repos, &filter.to_query()));
    }

    let mut tpl_ctx = base_context(&ctx);
    tpl_ctx.insert("page_type", "repositories");
    tpl_ctx.insert("repo_list", &repos);
    tpl_ctx.insert("sort", &sort);
    // Applied filter badges (current-filters.html).
    tpl_ctx.insert("filters", &filter.current_filters());
    tpl_ctx.insert("filters_url", &format!("{}/repos?", ctx.consts.root_url));
    tpl_ctx.insert("clear_url", &format!("{}/repos", ctx.consts.root_url));
    tpl_ctx.insert(
        "add_url",
        &filter
            .add_queries()
            .into_iter()
            .map(|(k, v)| (k, format!("{}/repos?{}", ctx.consts.root_url, v)))
            .collect::<std::collections::HashMap<_, _>>(),
    );

    tpl_ctx.insert(
        "sort_url",
        &format!("{}/repos?{}", ctx.consts.root_url, filter.to_query()),
    );
    tpl_ctx.insert(
        "total_packages",
        &repos.iter().map(|r| r.package_count).sum::<i64>(),
    );
    tpl_ctx.insert(
        "feed_url",
        &feed_url(
            &format!("{}/repos.xml", ctx.consts.root_url),
            &filter.to_query(),
        ),
    );

    render(&ctx, "repos.html", &mut tpl_ctx)
}

/// Standalone advanced search form.
pub async fn render_search_form(
    State(ctx): State<Arc<Ctx>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    Query(mut q): Query<PackageQuery>,
) -> Response {
    let mut tpl_ctx = base_context(&ctx);
    tpl_ctx.insert("page_type", "search-form");
    if let Some(slug) = params.get("repo") {
        match ctx.repo(slug) {
            Some(repo) => tpl_ctx.insert("repo", repo),
            None => return not_found(&ctx, "Unknown repository."),
        }
    }
    if let Err(e) = q.validate() {
        return render_message(&ctx, StatusCode::BAD_REQUEST, "Invalid search", e);
    }
    q.per_page = paginate(
        q.page,
        q.per_page,
        ctx.consts.site_max_per_page,
        ctx.consts.site_default_per_page,
    )
    .1;
    tpl_ctx.insert("q", &q);
    insert_search(&mut tpl_ctx, &q);
    render(&ctx, "search-form.html", &mut tpl_ctx)
}

/// Search results page. Takes the same query params as the JSON search API.
pub async fn render_search(
    State(ctx): State<Arc<Ctx>>,
    Extension(started): Extension<ReqStarted>,
    Path(repo_slug): Path<String>,
    RawQuery(raw_query): RawQuery,
    Query(mut q): Query<PackageQuery>,
) -> Response {
    // `/repos/{repo}.xml` returns results as an RSS feed.
    let (repo_slug, feed) = match repo_slug.strip_suffix(".xml") {
        Some(slug) => (slug, true),
        None => (repo_slug.as_str(), false),
    };

    let (repo, results) = match search_packages(
        &ctx,
        repo_slug,
        &mut q,
        ctx.consts.site_max_per_page,
        ctx.consts.site_default_per_page,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => return render_data_error(&ctx, e),
    };

    let raw_query = raw_query.unwrap_or_default();
    if feed {
        return render_feed(render_packages_rss(
            &ctx,
            &repo,
            &results,
            q.search().0,
            &raw_query,
        ));
    }

    let mut tpl_ctx = base_context(&ctx);
    tpl_ctx.insert("page_type", "search");
    tpl_ctx.insert("repo", &repo);
    tpl_ctx.insert("q", &q);
    insert_search(&mut tpl_ctx, &q);
    tpl_ctx.insert("results", &results);
    let package_urls: std::collections::HashMap<_, _> = results
        .packages
        .iter()
        .filter_map(|pkg| {
            repo.pkg_url_template
                .as_deref()
                .and_then(|tpl| package_url(tpl, pkg))
                .map(|url| (pkg.slug.as_str(), url))
        })
        .collect();
    tpl_ctx.insert("package_urls", &package_urls);

    // Current filters.
    let base_url = format!("{}/repos/{}", ctx.consts.root_url, repo.slug);
    let term_query = q.to_query();
    tpl_ctx.insert("filters", &q.applied_filters());
    tpl_ctx.insert("filters_url", &format!("{}?", base_url));
    tpl_ctx.insert(
        "clear_url",
        &if term_query.is_empty() {
            base_url.clone()
        } else {
            format!("{}?{}", base_url, term_query)
        },
    );
    tpl_ctx.insert(
        "add_url",
        &q.add_queries()
            .into_iter()
            .map(|(k, v)| (k, format!("{}?{}", base_url, v)))
            .collect::<std::collections::HashMap<_, _>>(),
    );

    tpl_ctx.insert("render_time", &fmt_duration(started.0.elapsed()));

    // Prefix that pagination links append their own cursor or page param to,
    // retaining all the other search params.
    tpl_ctx.insert(
        "pg_url",
        &format!(
            "{}/repos/{}?{}",
            ctx.consts.root_url,
            repo.slug,
            strip_pagination(&raw_query)
        ),
    );

    // Feed URL to render on the page.
    tpl_ctx.insert(
        "feed_url",
        &feed_url(&format!("{}.xml", base_url), &raw_query),
    );

    render(&ctx, "results.html", &mut tpl_ctx)
}

/// Individual package page.
pub async fn render_package(
    State(ctx): State<Arc<Ctx>>,
    Path((repo_slug, pkg_slug)): Path<(String, String)>,
    uri: Uri,
) -> Response {
    let (repo, pkg) = match get_package_by_slug(&ctx, &repo_slug, &pkg_slug).await {
        Ok(result) => result,
        Err(e) => return render_data_error(&ctx, e),
    };

    let repo_url = format!("{}/repos/{}", ctx.consts.root_url, repo.slug);
    let item = package_item(&repo_url, &pkg);
    let feed_url = format!("{}/feed.xml", item.link);
    if uri.path().ends_with("/feed.xml") {
        return render_feed(Feed {
            title: format!("{} · {} - pkg.bot", pkg.name, repo.name),
            description: format!("Updates to {} in {}.", pkg.name, repo.name),
            link: item.link.clone(),
            self_link: feed_url,
            items: vec![item],
        });
    }

    let mut tpl_ctx = base_context(&ctx);
    tpl_ctx.insert("page_type", "package");
    tpl_ctx.insert("feed_url", &feed_url);
    tpl_ctx.insert("repo", &repo);

    // The package's pages on the repo's own website.
    for (key, tpl) in [
        ("pkg_url", &repo.pkg_url_template),
        ("source_url", &repo.source_url_template),
    ] {
        if let Some(url) = tpl.as_deref().and_then(|t| package_url(t, &pkg)) {
            tpl_ctx.insert(key, &url);
        }
    }

    tpl_ctx.insert("pkg", &pkg);

    render(&ctx, "package.html", &mut tpl_ctx)
}

/// Render shared lookup and validation errors for the site.
fn render_data_error(ctx: &Ctx, err: ApiErr) -> Response {
    match err.status {
        StatusCode::NOT_FOUND => not_found(ctx, &err.message),
        StatusCode::BAD_REQUEST => render_message(ctx, err.status, "Invalid search", &err.message),
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT => {
            render_message(ctx, err.status, "Search unavailable", &err.message)
        }
        _ => {
            log::error!("error fetching packages: {}", err.message);
            render_message(ctx, err.status, "Error", "Error fetching packages.")
        }
    }
}

/// Expand a repository URL using the same fields on detail and results pages.
fn package_url(template: &str, pkg: &Package) -> Option<String> {
    let fields = url_template::Fields {
        slug: &pkg.slug,
        package: Some(pkg.package.as_str()),
        name: &pkg.name,
        pkg_base: pkg.pkg_base.as_deref(),
        version: pkg.version.as_deref(),
        subrepo: pkg.subrepo.as_deref(),
        arch: None,
    };

    url_template::expand(template, &fields)
}

/// Get the "latest" repos from each distro group, eg: Fedora 44, Fedora 43 .. pick the latest.
/// This is used to populate the "latest repos" dropdown in the search bar.
///
/// Split slugs by underscores, group by the first term (eg: fedora_44, fedora_43 -> fedora),
/// pick the highest numeric version within each group. Also include unstable repos.
fn get_latest_repos(repos: &[Repo]) -> Vec<&Repo> {
    fn release(repo: &Repo) -> (String, Vec<u64>, u8) {
        let parts: Vec<_> = repo.slug.split('_').collect();
        let suffix = &parts[1..];
        let version = suffix
            .iter()
            .skip_while(|p| p.trim_start_matches('v').parse::<u64>().is_err())
            .map_while(|p| p.trim_start_matches('v').parse::<u64>().ok())
            .collect();
        let channel = if suffix.contains(&"stable") {
            3
        } else if suffix.contains(&"rolling") {
            2
        } else if suffix.contains(&"oldstable") {
            0
        } else {
            1
        };
        (parts[0].to_owned(), version, channel)
    }

    let mut latest = std::collections::HashMap::new();
    for repo in repos {
        let (group, version, channel) = release(repo);
        let rank = (
            version,
            channel,
            &repo.created_at,
            std::cmp::Reverse(&repo.slug),
        );
        let entry = latest.entry(group).or_insert((repo, rank.clone()));
        if rank > entry.1 {
            *entry = (repo, rank);
        }
    }
    // Preserve the alphabetical ordering of the full list.
    repos
        .iter()
        .filter(|repo| {
            repo.slug.split('_').any(|part| part == "unstable")
                || latest.values().any(|(r, _)| r.id == repo.id)
        })
        .collect()
}

/// Template context common to all pages.
fn base_context(ctx: &Ctx) -> tera::Context {
    let mut tpl_ctx = tera::Context::new();
    tpl_ctx.insert("consts", &ctx.consts);
    tpl_ctx.insert("repos", &ctx.repos);
    tpl_ctx.insert(
        "latest_repos",
        LATEST_REPOS
            .get()
            .expect("latest repos initialized at startup"),
    );
    tpl_ctx.insert("asset_ver", &ctx.asset_ver);

    // The search form is on every page. Give it an empty query and the first
    // repo to start with. Pages that have a query and a repo of their own
    // overwrite these.
    tpl_ctx.insert("q", &PackageQuery::default());
    insert_search(&mut tpl_ctx, &PackageQuery::default());
    if let Some(repo) = ctx.repos.first() {
        tpl_ctx.insert("repo", repo);
    }

    tpl_ctx
}

/// The search term and its scope, which the form and result links are built
/// from without having to know which of the two params was used.
fn insert_search(tpl_ctx: &mut tera::Context, q: &PackageQuery) {
    let (term, name_only) = q.search();
    tpl_ctx.insert("term", term);
    tpl_ctx.insert("scope", if name_only { "name" } else { "q" });
}

/// Render a template into an HTML response.
fn render(ctx: &Ctx, tpl: &str, tpl_ctx: &mut tera::Context) -> Response {
    let Some(site) = &ctx.site else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    match site.tpl.render(tpl, tpl_ctx) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            log::error!("error rendering {}: {}", tpl, crate::init::err_chain(&e));

            (StatusCode::INTERNAL_SERVER_ERROR, "error rendering page").into_response()
        }
    }
}

/// Render the generic message page.
fn render_message(ctx: &Ctx, status: StatusCode, title: &str, message: &str) -> Response {
    let mut tpl_ctx = base_context(ctx);
    tpl_ctx.insert("page_type", "message");
    tpl_ctx.insert("title", title);
    tpl_ctx.insert("message", message);

    (status, render(ctx, "message.html", &mut tpl_ctx)).into_response()
}

fn not_found(ctx: &Ctx, message: &str) -> Response {
    render_message(ctx, StatusCode::NOT_FOUND, "Not found", message)
}

/// Render the RSS feed for repo lists.
fn render_repos_rss(ctx: &Ctx, repos: &[Repo], query: &str) -> Feed {
    let link = format!("{}/repos", ctx.consts.root_url);

    Feed {
        title: "Repositories - pkg.bot".into(),
        description: format!(
            "All {} Linux package repositories indexed here, with their package and maintainer counts.",
            repos.len()
        ),
        self_link: feed_url(&format!("{}.xml", link), query),
        items: repos
            .iter()
            .map(|r| {
                let url = format!("{}/repos/{}", ctx.consts.root_url, r.slug);

                Item {
                    title: match &r.branch {
                        Some(b) => format!("{} ({})", r.name, b),
                        None => r.name.clone(),
                    },
                    description: format!(
                        "{} packages and {} maintainers, managed with {}.",
                        r.package_count, r.num_maintainers, r.manager
                    ),
                    guid: url.clone(),
                    permalink: true,
                    link: url,
                    categories: [Some(&r.family), r.distro.as_ref()]
                        .into_iter()
                        .flatten()
                        .cloned()
                        .collect(),
                    pub_date: r.updated_at.clone(),
                }
            })
            .collect(),
        link,
    }
}

/// Render the RSS feed for package listings within a repository.
fn render_packages_rss(
    ctx: &Ctx,
    repo: &Repo,
    results: &PackageResults,
    term: &str,
    query: &str,
) -> Feed {
    let link = format!("{}/repos/{}", ctx.consts.root_url, repo.slug);

    Feed {
        title: if term.is_empty() {
            format!("{} packages - pkg.bot", repo.name)
        } else {
            format!("{} · {} packages - pkg.bot", term, repo.name)
        },
        description: if term.is_empty() {
            format!("Packages in {}.", repo.name)
        } else {
            format!("Packages matching \"{}\" in {}.", term, repo.name)
        },
        self_link: feed_url(&format!("{}.xml", link), query),
        items: results
            .packages
            .iter()
            .map(|p| package_item(&link, p))
            .collect(),
        link,
    }
}

fn package_item(repo_url: &str, p: &Package) -> Item {
    let url = format!("{}/{}", repo_url, urlencoding::encode(&p.slug));

    Item {
        title: match &p.version {
            Some(v) => format!("{} {}", p.name, v),
            None => p.name.clone(),
        },
        // Make version a part of guid so that updates show up as new items in the feed.
        guid: match &p.version {
            Some(v) => format!("{}@{}", url, v),
            None => url.clone(),
        },
        permalink: false,
        link: url,
        description: p
            .excerpt
            .clone()
            .or_else(|| p.description.clone())
            .unwrap_or_default(),
        categories: p.keywords.0.clone(),
        pub_date: p.updated_at.clone(),
    }
}

/// Serialize a feed into an RSS response.
fn render_feed(feed: Feed) -> Response {
    match feed.to_xml() {
        Ok(xml) => (
            [(header::CONTENT_TYPE, "application/rss+xml; charset=utf-8")],
            xml,
        )
            .into_response(),
        Err(e) => {
            log::error!("error rendering feed: {}", e);

            (StatusCode::INTERNAL_SERVER_ERROR, "error rendering feed").into_response()
        }
    }
}

/// Build a feed's own URL.
fn feed_url(base: &str, query: &str) -> String {
    match query.trim_end_matches('&') {
        "" => base.to_string(),
        q => format!("{}?{}", base, q),
    }
}

/// Drop pagination params from a raw query string so that pagination links can
/// append their own. Eg: "q=vim&page=3" => "q=vim&"
fn strip_pagination(raw: &str) -> String {
    raw.split('&')
        .filter(|p| {
            !p.is_empty()
                && !["page=", "after=", "before="]
                    .iter()
                    .any(|k| p.starts_with(k))
        })
        .flat_map(|p| [p, "&"])
        .collect()
}

/// Format a duration as `1s3ms`, `10ms`, or `250us`.
fn fmt_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms >= 1000 {
        format!("{}s{}ms", ms / 1000, ms % 1000)
    } else if ms > 0 {
        format!("{}ms", ms)
    } else {
        format!("{}us", d.as_micros())
    }
}

#[cfg(test)]
mod search_repo_tests {
    use super::*;

    #[test]
    fn compact_dropdown_renders_latest_and_view_all() {
        let repos = vec![
            Repo {
                id: 1,
                slug: "fedora_43".into(),
                name: "Fedora 43".into(),
                ..Repo::default()
            },
            Repo {
                id: 2,
                slug: "fedora_44".into(),
                name: "Fedora 44".into(),
                ..Repo::default()
            },
        ];
        let mut context = tera::Context::new();
        context.insert("repos", &repos);
        context.insert("latest_repos", &get_latest_repos(&repos));
        context.insert("repo", &repos[0]);
        context.insert("term", "");
        context.insert("advanced_form", &false);
        context.insert("asset_ver", "test");
        context.insert("consts", &serde_json::json!({"root_url": "/prefix"}));
        let html = tera::Tera::one_off(
            include_str!("../../site/partials/search-inputs.html"),
            &context,
            true,
        )
        .unwrap();
        assert!(!html.contains("Fedora 43"));
        assert!(html.contains("Fedora 44"));
        assert!(html.contains("disabled selected>Select repository"));
        assert!(html.contains("data-view-all-repos>View all repos (2)</option>"));
        assert!(html.contains("/prefix/search"));
    }

    #[test]
    fn latest_versions_and_channels_keep_independent_repos() {
        let slugs = [
            "alpine_3_9",
            "alpine_3_24",
            "alpine_edge",
            "arch",
            "aur",
            "debian_13",
            "debian_14",
            "debian_unstable",
            "epel_9",
            "epel_10",
            "fedora_44",
            "fedora_rawhide",
            "manjaro_testing",
            "manjaro_stable",
            "manjaro_unstable",
            "nix_stable_26_05",
            "nix_unstable",
            "ubuntu_26_10",
            "ubuntu_26_10_proposed",
        ];
        let repos: Vec<_> = slugs
            .iter()
            .enumerate()
            .map(|(id, slug)| Repo {
                id: id as i64,
                slug: (*slug).into(),
                ..Repo::default()
            })
            .collect();
        let selected: Vec<_> = get_latest_repos(&repos)
            .iter()
            .map(|r| r.slug.as_str())
            .collect();
        assert_eq!(
            selected,
            [
                "alpine_3_24",
                "arch",
                "aur",
                "debian_14",
                "debian_unstable",
                "epel_10",
                "fedora_44",
                "manjaro_stable",
                "manjaro_unstable",
                "nix_stable_26_05",
                "nix_unstable",
                "ubuntu_26_10",
            ]
        );
    }
}
