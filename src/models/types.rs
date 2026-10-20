use serde::{Deserialize, Serialize};
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
    Decode, Encode, FromRow, Sqlite, Type,
};

use super::url_template;

/// Status of a package (eg: active, deleted etc.).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum PackageStatus {
    #[default]
    Unprocessed,
    Newest,
    Outdated,
    Ignored,
    Unique,
    Devel,
    Legacy,
    Incorrect,
    Untrusted,
    Noscheme,
    Rolling,
}

impl PackageStatus {
    pub fn from_versionclass(value: i32) -> Self {
        match value {
            1 => Self::Newest,
            2 => Self::Outdated,
            3 => Self::Ignored,
            4 => Self::Unique,
            5 => Self::Devel,
            6 => Self::Legacy,
            7 => Self::Incorrect,
            8 => Self::Untrusted,
            9 => Self::Noscheme,
            10 => Self::Rolling,
            _ => Self::Unprocessed,
        }
    }
}

/// JSON array wrapper for SQLite TEXT columns storing JSON arrays.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JsonArray<T>(pub Vec<T>);

pub type StringArray = JsonArray<String>;

impl<T> From<Vec<T>> for JsonArray<T> {
    fn from(v: Vec<T>) -> Self {
        Self(v)
    }
}

impl<T> Type<Sqlite> for JsonArray<T> {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }
}

impl<'q, T: Serialize> Encode<'q, Sqlite> for JsonArray<T> {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        let json = serde_json::to_string(&self.0).unwrap_or_else(|_| "[]".to_string());
        <String as Encode<Sqlite>>::encode(json, buf)
    }
}

impl<'r, T: serde::de::DeserializeOwned> Decode<'r, Sqlite> for JsonArray<T> {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let s = <&str as Decode<Sqlite>>::decode(value)?;
        if s.is_empty() {
            return Ok(Self(Vec::new()));
        }
        Ok(Self(serde_json::from_str(s)?))
    }
}

/// Maintainer of a package.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Maintainer {
    pub slug: String,
    pub handle: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
}

/// JSON object stored as a String internally, but serialized as raw JSON.
/// Kind of emulates json.RawMessage behaviour in Go.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(from = "serde_json::Value")]
pub struct JsonString(pub String);

impl From<serde_json::Value> for JsonString {
    fn from(v: serde_json::Value) -> Self {
        Self(v.to_string())
    }
}

impl serde::Serialize for JsonString {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.0.is_empty() || self.0 == "{}" {
            // Serialize as empty object.
            use serde::ser::SerializeMap;
            serializer.serialize_map(Some(0))?.end()
        } else {
            // Parse the string as JSON and then serialize it.
            let value: serde_json::Value = serde_json::from_str(&self.0)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            value.serialize(serializer)
        }
    }
}

impl Type<Sqlite> for JsonString {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }
}

impl<'q> Encode<'q, Sqlite> for JsonString {
    fn encode_by_ref(
        &self,
        buf: &mut Vec<SqliteArgumentValue<'q>>,
    ) -> std::result::Result<IsNull, BoxDynError> {
        let s = if self.0.is_empty() {
            "{}".to_string()
        } else {
            self.0.clone()
        };
        <String as Encode<Sqlite>>::encode(s, buf)
    }
}

impl<'r> Decode<'r, Sqlite> for JsonString {
    fn decode(value: SqliteValueRef<'r>) -> std::result::Result<Self, BoxDynError> {
        let s = <String as Decode<Sqlite>>::decode(value)?;
        Ok(Self(s))
    }
}

/// Sort order.
pub const ORDER_ASC: &str = "asc";
pub const ORDER_DESC: &str = "desc";

/// Columns the repo listings can be sorted by.
pub const REPO_SORT_FIELDS: [&str; 6] = [
    "name",
    "distro",
    "manager",
    "package_count",
    "num_maintainers",
    "updated_at",
];

/// Repo listing filters.
pub const REPO_FILTERS: [&str; 3] = ["family", "distro", "manager"];

/// Package search filters.
pub const PACKAGE_FILTERS: [&str; 9] = [
    "license",
    "tag",
    "maintainer",
    "group",
    "platform",
    "status",
    "is_nonfree",
    "version",
    "updated_at",
];

/// `?order_by=&order=` on a listing page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub order_by: String,
    pub order: String,
}

impl Sort {
    /// Sort ascending by `field`.
    pub fn asc(field: &str) -> Self {
        Self {
            order_by: field.to_string(),
            order: ORDER_ASC.to_string(),
        }
    }

    /// Validate sort fields.
    pub fn clamp(mut self, fields: &[&str], default: &str) -> Self {
        if !fields.contains(&self.order_by.as_str()) {
            self.order_by = default.to_string();
        }
        self.order = if self.order.eq_ignore_ascii_case(ORDER_DESC) {
            ORDER_DESC
        } else {
            ORDER_ASC
        }
        .to_string();

        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoQuery {
    pub family: String,
    pub distro: String,
    pub manager: String,
}

impl RepoQuery {
    /// Get non-empty filters as (key, value) pairs.
    pub fn filters(&self) -> Vec<(&'static str, &str)> {
        [
            ("family", self.family.trim()),
            ("distro", self.distro.trim()),
            ("manager", self.manager.trim()),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
    }

    /// Get currently applied filters.
    pub fn current_filters(&self) -> Vec<AppliedFilter> {
        applied_filters(&self.filters(), "")
    }

    /// Query string prefixes by filter keys that filter links on a page can append to.
    pub fn add_queries(&self) -> std::collections::HashMap<&'static str, String> {
        REPO_FILTERS
            .iter()
            .map(|k| (*k, to_query(&self.filters(), k)))
            .collect()
    }

    pub fn to_query(&self) -> String {
        to_query(&self.filters(), "")
    }
}

/// List of autocomplete values for the aPI.
#[derive(Debug, Default)]
pub struct Suggestions {
    values: Vec<(String, String)>,
}

impl Suggestions {
    pub fn new(values: Vec<String>) -> Self {
        Self {
            values: values
                .into_iter()
                .map(|v| {
                    let lower = v.to_lowercase();
                    (v, lower)
                })
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Match `q`, prefix matches first, then substring. Multiple words are matched as AND.
    pub fn query(&self, q: &str, limit: usize) -> Vec<String> {
        let q = q.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        if words.is_empty() {
            return self
                .values
                .iter()
                .take(limit)
                .map(|(v, _)| v.clone())
                .collect();
        }

        let mut prefix: Vec<&str> = Vec::new();
        let mut substr: Vec<&str> = Vec::new();
        for (val, lower) in &self.values {
            if !words.iter().all(|w| lower.contains(w)) {
                continue;
            }

            if lower.starts_with(words[0]) {
                prefix.push(val);
                if prefix.len() >= limit {
                    break;
                }
            } else if substr.len() < limit {
                substr.push(val);
            }
        }

        prefix
            .into_iter()
            .chain(substr)
            .take(limit)
            .map(|v| v.to_string())
            .collect()
    }
}

/// A package repository (a distro's repo, channel or branch).
#[derive(Debug, Clone, Default, Serialize, FromRow)]
pub struct Repo {
    #[serde(skip)]
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub family: String,
    pub manager: String,
    pub distro: Option<String>,
    pub branch: Option<String>,

    pub homepage_url: Option<String>,
    pub links: JsonString,

    #[serde(skip)]
    pub pkg_url_template: Option<String>,

    #[serde(skip)]
    pub source_url_template: Option<String>,

    #[serde(skip)]
    pub meta: JsonString,
    pub brand_color: Option<String>,

    #[serde(skip)]
    pub score: f64,
    pub package_count: i64,
    pub num_packages: i32,
    pub num_maintainers: i32,

    pub created_at: String,
    pub updated_at: String,
}

/// A package in a repository.
#[derive(Debug, Clone, Default, Serialize, FromRow)]
pub struct Package {
    #[serde(skip)]
    pub id: i64,

    /// Slug of the repo the package belongs to.
    pub repo: String,

    pub slug: String,
    pub name: String,
    pub package: String,

    #[serde(skip)]
    pub name_norm: String,
    pub excerpt: Option<String>,
    pub description: Option<String>,
    pub pkg_base: Option<String>,
    pub subrepo: Option<String>,
    pub project_name: Option<String>,
    pub binary_names: StringArray,

    pub version: Option<String>,

    #[serde(skip)]
    pub version_norm: Option<String>,

    pub homepage_url: Option<String>,

    pub licenses: StringArray,
    pub is_nonfree: Option<bool>,
    pub platforms: StringArray,

    pub groups: StringArray,
    pub keywords: StringArray,

    pub status: PackageStatus,

    #[serde(skip)]
    pub score: f64,

    pub meta: JsonString,

    pub maintainers: JsonArray<Maintainer>,

    pub created_at: String,
    pub updated_at: String,
}

/// Package search query parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageQuery {
    /// Search across every indexed field. Mutually exclusive with `name`.
    #[serde(default)]
    pub q: String,

    /// Search restricted to the package name and slug. Mutually exclusive with `q`.
    #[serde(default)]
    pub name: String,

    /// Filters. Each takes a single value, eg: `license=MIT&status=outdated`.
    #[serde(default)]
    pub maintainer: String,

    #[serde(default)]
    pub tag: String,

    #[serde(default)]
    pub license: String,

    #[serde(default)]
    pub group: String,

    #[serde(default)]
    pub platform: String,

    #[serde(default)]
    pub status: String,

    #[serde(default)]
    pub is_nonfree: String,

    #[serde(default)]
    pub version: String,

    #[serde(default)]
    pub updated_at: String,

    /// Keyset pagination fields.
    #[serde(default)]
    pub after: String,

    #[serde(default)]
    pub before: String,

    #[serde(default)]
    pub page: i32,

    #[serde(default)]
    pub per_page: i32,

    // Internal fields (not from HTTP query).
    #[serde(skip)]
    pub repo_id: i64,

    #[serde(skip)]
    pub offset: i32,

    #[serde(skip)]
    pub limit: i32,
}

impl PackageQuery {
    /// Reject combined `q` and `name` searches and convert `is_nonfree` to "1"/"0".
    pub fn validate(&mut self) -> Result<(), &'static str> {
        if !self.q.trim().is_empty() && !self.name.trim().is_empty() {
            return Err("q and name cannot be used together");
        }

        self.is_nonfree = match self.is_nonfree.trim() {
            "" => String::new(),
            "1" | "true" => "1".into(),
            "0" | "false" => "0".into(),
            _ => return Err("is_nonfree must be true or false"),
        };

        // Validate semver and date strings with optional > and < comparators.
        self.version.retain(|c| !c.is_whitespace());
        self.updated_at.retain(|c| !c.is_whitespace());
        if !self.version.is_empty() {
            let (_, value) = get_comparator(&self.version);
            let parts: Vec<_> = value.split('.').collect();
            if parts.len() > 3
                || parts.iter().any(|p| {
                    p.is_empty()
                        || !p.bytes().all(|c| c.is_ascii_digit())
                        || (p.len() > 1 && p.starts_with('0'))
                })
            {
                return Err("version must be x, x.y, or x.y.z optionally prefixed by > or <");
            }
        }
        if !self.updated_at.is_empty() {
            let (_, value) = get_comparator(&self.updated_at);
            if value.len() != 10
                || !value.bytes().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        c == b'-'
                    } else {
                        c.is_ascii_digit()
                    }
                })
                || chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err()
            {
                return Err("updated_at must be yyyy-mm-dd, optionally prefixed by > or <");
            }
        }
        Ok(())
    }

    /// Return nonempty filters as (kind, value) pairs. Platform is checked separately.
    pub fn facets(&self) -> Vec<(&'static str, &str)> {
        [
            ("license", self.license.trim()),
            ("tag", self.tag.trim()),
            ("maintainer", self.maintainer.trim()),
            ("group", self.group.trim()),
            ("status", self.status.trim()),
            ("is_nonfree", self.is_nonfree.as_str()),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
    }

    /// Get every non-empty filter as (key, value) pairs for display.
    pub fn filters(&self) -> Vec<(&'static str, &str)> {
        [
            ("version", self.version.as_str()),
            ("updated_at", self.updated_at.as_str()),
            ("license", self.license.trim()),
            ("tag", self.tag.trim()),
            ("maintainer", self.maintainer.trim()),
            ("group", self.group.trim()),
            ("platform", self.platform.trim()),
            ("status", self.status.trim()),
            (
                "is_nonfree",
                match self.is_nonfree.as_str() {
                    "1" => "true",
                    "0" => "false",
                    _ => "",
                },
            ),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
    }

    /// Whether the listing has any filters.
    pub fn has_filters(&self) -> bool {
        !self.filters().is_empty()
    }

    /// Get applied filters, each with the query string that drops it.
    pub fn applied_filters(&self) -> Vec<AppliedFilter> {
        applied_filters(&self.filters(), &self.to_query())
    }

    /// Add queries for each filter, keyed by the filter name.
    pub fn add_queries(&self) -> std::collections::HashMap<&'static str, String> {
        PACKAGE_FILTERS
            .iter()
            .map(|k| (*k, self.to_query_excluding(k)))
            .collect()
    }

    /// Get the search term as a query string eg: `q=vim&`.
    pub fn to_query(&self) -> String {
        let (term, name_only) = self.search();
        if term.is_empty() {
            return String::new();
        }

        format!(
            "{}={}&",
            if name_only { "name" } else { "q" },
            url_template::urlencode(term)
        )
    }

    /// Query string with the search term and all the filters except `excl`.
    pub fn to_query_excluding(&self, excl: &str) -> String {
        self.to_query() + &to_query(&self.filters(), excl)
    }

    pub fn search(&self) -> (&str, bool) {
        if self.name.trim().is_empty() {
            (self.q.trim(), false)
        } else {
            (self.name.trim(), true)
        }
    }
}

/// A filter applied to a listing with a copy of the query string without it (used to 'clear' a particular filter).
#[derive(Debug, Clone, Serialize)]
pub struct AppliedFilter {
    pub key: &'static str,
    pub value: String,
    pub query: String,
}

/// Convert filter [key, value] filters into badges with a query string
/// without itself to get the 'clear' link.
fn applied_filters(filters: &[(&'static str, &str)], prefix: &str) -> Vec<AppliedFilter> {
    filters
        .iter()
        .map(|(key, value)| AppliedFilter {
            key,
            value: (*value).to_string(),
            query: prefix.to_string() + &to_query(filters, key),
        })
        .collect()
}

/// Convert [key, value] filters to a query string, excluding the specified key.
fn to_query(filters: &[(&'static str, &str)], excl: &str) -> String {
    filters
        .iter()
        .filter(|(k, _)| *k != excl)
        .map(|(k, v)| format!("{}={}&", k, url_template::urlencode(v)))
        .collect()
}

/// Package search results.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PackageResults {
    pub packages: Vec<Package>,
    pub per_page: i32,
    /// Package count for browsing. Omitted for search results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,

    /// Set when `total` hit the listing count limit and is really "total or more".
    pub total_capped: bool,

    // Offset pagination (search).
    pub page: i32,
    pub has_more: bool,

    // Keyset pagination (listing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
}

/// Keyset pagination key serialized as `id:name`.
#[derive(Debug, Clone, Default)]
pub struct Cursor {
    pub id: i64,
    pub name: String,
}

impl Cursor {
    pub fn of(p: &Package) -> String {
        format!("{}:{}", p.id, p.name)
    }

    /// Parse a keyset pagination string.
    pub fn parse(s: &str) -> Self {
        match s.split_once(':') {
            Some((id, name)) => Self {
                id: id.parse().unwrap_or_default(),
                name: name.to_string(),
            },
            None => Self::default(),
        }
    }
}

/// Application configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub import: ImportConfig,

    pub app: AppConfig,
    pub db: DbConfig,

    #[serde(default)]
    pub api_results: ApiResultsConfig,

    #[serde(default)]
    pub site_results: SiteResultsConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub address: String,

    #[serde(default)]
    pub root_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DbConfig {
    pub max_conns: u32,
    pub query_timeout_ms: u64,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            max_conns: 2,
            query_timeout_ms: 3000,
        }
    }
}

impl DbConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_conns == 0 {
            return Err("db.max_conns must be positive");
        }
        if !(1..=60000).contains(&self.query_timeout_ms) {
            return Err("db.query_timeout_ms must be between 1 and 60000");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiResultsConfig {
    #[serde(default = "default_api_per_page")]
    pub per_page: i32,
    #[serde(default = "default_api_max_per_page")]
    pub max_per_page: i32,
}

fn default_api_per_page() -> i32 {
    20
}

fn default_api_max_per_page() -> i32 {
    50
}

impl Default for ApiResultsConfig {
    fn default() -> Self {
        Self {
            per_page: default_api_per_page(),
            max_per_page: default_api_max_per_page(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SiteResultsConfig {
    #[serde(default = "default_site_per_page")]
    pub per_page: i32,
    #[serde(default = "default_site_max_per_page")]
    pub max_per_page: i32,
}

fn default_site_per_page() -> i32 {
    50
}

fn default_site_max_per_page() -> i32 {
    100
}

impl Default for SiteResultsConfig {
    fn default() -> Self {
        Self {
            per_page: default_site_per_page(),
            max_per_page: default_site_max_per_page(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImportConfig {
    #[serde(default)]
    pub families: Vec<String>,

    #[serde(default)]
    pub min_packages: i32,
}

/// Extract < or > comparator from a string.
pub fn get_comparator(value: &str) -> (&'static str, &str) {
    if let Some(value) = value.strip_prefix('>') {
        (">", value)
    } else if let Some(value) = value.strip_prefix('<') {
        ("<", value)
    } else {
        ("=", value)
    }
}

/// Zero-pad the first 3 numeric parts of a version into a lexically sortable string.
pub fn normalize_version(v: &str) -> Option<String> {
    if v.is_empty() {
        return None;
    }

    // Remove any numeric epoch prefix ("2:1.0" -> "1.0").
    let v = v
        .split_once(':')
        .filter(|(epoch, _)| !epoch.is_empty() && epoch.chars().all(|c| c.is_ascii_digit()))
        .map_or(v, |(_, rest)| rest);

    let mut parts: Vec<String> = v
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(3)
        .map(|s| format!("{:05}", s.parse::<u64>().unwrap_or(u64::MAX).min(99999)))
        .collect();
    parts.resize(3, "00000".to_string());

    Some(parts.join("."))
}

#[cfg(test)]
mod comparison_tests {
    use super::*;

    #[test]
    fn validate_comparisons() {
        for value in ["1", ">1.2", "<1.2.3", "0.0.0", " > 1 . 2 "] {
            let mut q = PackageQuery {
                version: value.into(),
                ..Default::default()
            };
            assert!(q.validate().is_ok(), "{value}");
            assert!(!q.version.contains(' '));
        }
        for value in [
            "=1", ">=1", "<=1", ">", "1.", "1.2.3.4", "v1", "1.2-beta", "01", "1 OR 1=1",
        ] {
            let mut q = PackageQuery {
                version: value.into(),
                ..Default::default()
            };
            assert!(q.validate().is_err(), "{value}");
        }
        for value in ["2024-02-29", ">2025-01-01", "< 2026-09-14"] {
            let mut q = PackageQuery {
                updated_at: value.into(),
                ..Default::default()
            };
            assert!(q.validate().is_ok(), "{value}");
        }
        for value in [
            "2025-02-29",
            "2026-13-01",
            "2026-1-01",
            "2026-01-01T00:00:00Z",
            "=2026-01-01",
            ">=2026-01-01",
            ">",
        ] {
            let mut q = PackageQuery {
                updated_at: value.into(),
                ..Default::default()
            };
            assert!(q.validate().is_err(), "{value}");
        }
        let mut q = PackageQuery {
            version: " > 2.1 ".into(),
            updated_at: "2026-09-14".into(),
            ..Default::default()
        };
        q.validate().unwrap();
        assert!(q.to_query_excluding("").contains("version=%3E2.1&"));
        assert!(q
            .to_query_excluding("version")
            .contains("updated_at=2026-09-14&"));
        assert_eq!(
            normalize_version(get_comparator(&q.version).1).as_deref(),
            Some("00002.00001.00000")
        );
    }
}
