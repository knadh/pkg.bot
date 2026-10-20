pub mod api;
mod format;
pub mod site;

pub use format::{respond, ApiFormat};

use std::{path::PathBuf, sync::Arc, time::Instant};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use tera::Tera;

use crate::{
    manager::Manager,
    models::{Repo, Suggestions},
};

/// Request start time inserted by the `track_time` middleware.
#[derive(Clone, Copy)]
pub struct ReqStarted(pub Instant);

/// Application context passed to all handlers.
pub struct Ctx {
    pub mgr: Arc<Manager>,

    /// All repos in the database, loaded once on boot. New repos added by an
    /// indexer only show up on the next restart.
    pub repos: Vec<Repo>,

    /// Latest repository update timestamp, collected once on boot.
    pub last_updated: Option<String>,

    /// Autosuggestion fields.
    pub licenses: Suggestions,
    pub platforms: Suggestions,

    /// HTML SSR site templates. `None` if the `--site` isn't set.
    pub site: Option<Site>,

    pub consts: Consts,

    /// Random string appended to static asset URLs for cache busting.
    pub asset_ver: String,
}

impl Ctx {
    /// Get a repo by its slug.
    pub fn repo(&self, slug: &str) -> Option<&Repo> {
        self.repos.iter().find(|r| r.slug == slug)
    }
}

/// Server-rendered site loaded from the `--site` dir.
pub struct Site {
    pub tpl: Tera,
    pub path: PathBuf,
}

/// Application constants.
#[derive(Clone, Serialize)]
pub struct Consts {
    pub root_url: String,

    // API pagination settings.
    pub api_default_per_page: i32,
    pub api_max_per_page: i32,

    // Site pagination settings.
    pub site_default_per_page: i32,
    pub site_max_per_page: i32,
}

/// API response wrapper.
#[derive(Serialize)]
pub struct ApiResp<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub data: Option<T>,
}

impl<T: Serialize> IntoResponse for ApiResp<T> {
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}

pub fn json<T: Serialize>(data: T) -> ApiResp<T> {
    ApiResp {
        data: Some(data),
        message: None,
    }
}

/// API error type.
#[derive(Debug)]
pub struct ApiErr {
    pub message: String,
    pub status: StatusCode,
}

impl ApiErr {
    pub fn new(message: impl Into<String>, status: StatusCode) -> Self {
        Self {
            message: message.into(),
            status,
        }
    }
}

impl From<crate::manager::Error> for ApiErr {
    fn from(err: crate::manager::Error) -> Self {
        use crate::manager::Error;
        let status = match &err {
            Error::Busy => StatusCode::SERVICE_UNAVAILABLE,
            Error::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Error::NotFound => StatusCode::NOT_FOUND,
            Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(err.to_string(), status)
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        let json = Json(ApiResp::<()> {
            data: None,
            message: Some(self.message),
        });
        (self.status, json).into_response()
    }
}

pub type Result<T> = std::result::Result<T, ApiErr>;

/// Pagination helper.
pub fn paginate(
    page: i32,
    per_page: i32,
    max_per_page: i32,
    default_per_page: i32,
) -> (i32, i32, i32) {
    let page = if page < 1 { 1 } else { page };
    let per_page = if per_page < 1 {
        default_per_page
    } else if per_page > max_per_page {
        max_per_page
    } else {
        per_page
    };
    let offset = (page - 1) * per_page;
    (page, per_page, offset)
}
