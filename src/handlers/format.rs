use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

use super::{json, ApiErr, Result};
use crate::models::{Package, Repo};

/// CSV columns to include, in order.
pub trait CsvColumns {
    const COLUMNS: &'static [&'static str];
}

impl CsvColumns for Repo {
    const COLUMNS: &'static [&'static str] = &[
        "slug",
        "name",
        "distro",
        "family",
        "manager",
        "package_count",
        "num_packages",
        "updated_at",
        "homepage_url",
        "links",
    ];
}

impl CsvColumns for Package {
    const COLUMNS: &'static [&'static str] = &[
        "slug",
        "package",
        "version",
        "status",
        "updated_at",
        "homepage_url",
        "licenses",
        "maintainers",
        "excerpt",
    ];
}

impl CsvColumns for String {
    const COLUMNS: &'static [&'static str] = &["value"];
}

impl<T: CsvColumns> CsvColumns for Vec<T> {
    const COLUMNS: &'static [&'static str] = T::COLUMNS;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiFormat {
    Json,
    Csv,
}

impl<S: Send + Sync> FromRequestParts<S> for ApiFormat {
    type Rejection = ApiErr;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self> {
        // JSON response by default unless the `Accept` header specifically asks for CSV.
        let csv = parts
            .headers
            .get_all(header::ACCEPT)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|value| {
                let mime = value.split_once(';').map_or(value, |(mime, _)| mime);
                mime.trim().eq_ignore_ascii_case("text/csv")
            });

        Ok(if csv { Self::Csv } else { Self::Json })
    }
}

pub fn respond<T: Serialize + CsvColumns>(format: ApiFormat, data: T) -> Result<Response> {
    let response = match format {
        ApiFormat::Json => json(data).into_response(),
        ApiFormat::Csv => (
            [(header::CONTENT_TYPE, "text/csv; charset=utf-8")],
            write_csv(
                serde_json::to_value(&data).map_err(format_error)?,
                T::COLUMNS,
            )?,
        )
            .into_response(),
    };

    Ok(([(header::VARY, "Accept")], response).into_response())
}

/// Convert objects to CSV.
fn write_csv(data: Value, headers: &[&str]) -> Result<Vec<u8>> {
    let rows = match data {
        Value::Array(rows) => rows,
        row => vec![row],
    };
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut writer = csv::WriterBuilder::new()
        .delimiter(b'|')
        .from_writer(Vec::new());
    writer.write_record(headers).map_err(format_error)?;
    for row in &rows {
        writer
            .write_record(headers.iter().map(|key| {
                let value = if row.is_object() { &row[*key] } else { row };
                let text = match value {
                    Value::Array(links) if *key == "links" => links
                        .iter()
                        .filter_map(|link| link["url"].as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => cell(value),
                };

                esc_terminal_chars(&text)
            }))
            .map_err(format_error)?;
    }
    writer.into_inner().map_err(format_error)
}

fn format_error(err: impl std::fmt::Display) -> ApiErr {
    ApiErr::new(
        err.to_string(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    )
}

/// Flatten nested values to comma or space separated strings.
fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(cell).collect::<Vec<_>>().join(", "),
        Value::Object(fields) => fields
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| format!("{key}: {}", cell(value)))
            .collect::<Vec<_>>()
            .join(", "),
        _ => value.to_string(),
    }
}

/// Escape terminal characters.
fn esc_terminal_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\u007c"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.extend(c.escape_unicode()),
            _ => out.push(c),
        }
    }
    out
}
