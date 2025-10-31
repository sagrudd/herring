//! ENA client and data shaping utilities used by the `herring` binary.
//!
//! This module provides request construction, basic retry logic, and the
//! functions that fetch ONT runs either for a rolling window (`first_public`
//! **or** `last_updated`) or a fixed release window (`first_public` only).
//!
//! Network behavior (timeouts, TLS, retries) is centralized here.
//!
//! ## Environment variables
//! - `HERRING_INSECURE_TLS=1` — disable TLS validation (debug only)
//! - `HERRING_CA_BUNDLE=/path/to/ca.pem` — add custom CA roots
//! - `HERRING_TIMEOUT_SECS` — request timeout in seconds
//!
//! ## Errors
//! Functions return [`anyhow::Result`], wrapping transport and decode errors.

use anyhow::{bail, Context, Result};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{blocking::Client, Certificate, StatusCode};
use serde::Deserialize;
use std::{collections::HashSet, env, fs, thread, time::Duration};
use log::{debug, info, warn};

const PORTAL_BASE: &str = "https://www.ebi.ac.uk/ena/portal/api";

/// A single ENA `read_run` row returned by the search endpoint.
#[derive(Debug, Deserialize, Clone)]
pub struct RunRecord {
    /// Run accession (may be absent in some API responses).
    pub run_accession: Option<String>,
    /// Study accession this run belongs to.
    pub study_accession: String,
    /// Sample accession for this run.
    pub sample_accession: Option<String>,
    /// Base count for the run (string in API; parsed later as `u64`).
    pub base_count: Option<String>,
    /// Instrument model (e.g. "PromethION", "GridION", "MinION").
    pub instrument_model: Option<String>,
    /// ENA library strategy field.
    pub library_strategy: Option<String>,
    /// Scientific name as reported by ENA.
    pub scientific_name: Option<String>,
    /// First public date (YYYY-MM-DD).
    pub first_public: Option<String>,
    /// Study title (if provided on the run row).
    pub study_title: Option<String>,
}

/// Map raw instrument model → a normalized ONT platform label.
///
/// Returns one of: "PromethION", "GridION", "MinION", or "Oxford Nanopore".
pub fn map_platform(model: Option<&str>) -> &'static str {
    if let Some(m) = model {
        let m = m.to_ascii_lowercase();
        if m.contains("prometh") { return "PromethION"; }
        if m.contains("gridion") { return "GridION"; }
        if m.contains("minion") || m.contains("flongle") { return "MinION"; }
        if m.contains("ont") { return "Oxford Nanopore"; }
    }
    "Oxford Nanopore"
}

/// Map ENA `library_strategy` to a coarse sequencing type.
///
/// - Transcriptome bucket: `RNA-SEQ`, `TRANSCRIPTOME SEQUENCING`, `MRNA-SEQ`, `CDNA`
/// - Metagenome bucket: `METAGENOME`, `METATRANSCRIPTOME`
/// - Genome bucket: `WGS`, `WGA`, `HI-C`, `AMPLICON`, `AMPLICON SEQUENCING`
/// - Otherwise: lowercase of the provided value or `"other"`
pub fn map_strategy(s: &str) -> String {
    match s.to_ascii_uppercase().as_str() {
        "RNA-SEQ" | "TRANSCRIPTOME SEQUENCING" | "MRNA-SEQ" | "CDNA" => "transcriptome".to_string(),
        "METAGENOME" | "METATRANSCRIPTOME" => "metagenome".to_string(),
        "WGS" | "WGA" | "HI-C" | "AMPLICON" | "AMPLICON SEQUENCING" => "genome".to_string(),
        other => match other {
            "OTHER" => "other".to_string(),
            _ => other.to_ascii_lowercase(),
        },
    }
}

/// Construct a blocking HTTP client with optional TLS overrides and timeouts.
fn make_client(ua: &str) -> Result<Client> {
    let mut builder = Client::builder().user_agent(ua);
    if env::var("HERRING_INSECURE_TLS").as_deref() == Ok("1") {
        builder = builder.danger_accept_invalid_certs(true);
        warn!("TLS validation disabled via HERRING_INSECURE_TLS=1");
    }
    if let Ok(p) = env::var("HERRING_CA_BUNDLE") {
        let pem = fs::read(&p)?;
        builder = builder.add_root_certificate(Certificate::from_pem(&pem)?);
        info!("added extra root certificate(s) from {}", p);
    }
    let timeout = env::var("HERRING_TIMEOUT_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(30);
    builder = builder.timeout(Duration::from_secs(timeout));
    info!("HTTP client timeout = {}s", timeout);
    Ok(builder.build()?)
}

/// Send a GET with basic **exponential backoff** on common retryable statuses.
///
/// Retryable: `429, 500, 502, 503, 504`. Non-retryable statuses return immediately.
fn request_with_retries(client: &Client, url: &str) -> Result<reqwest::blocking::Response> {
    let mut delay = Duration::from_millis(400);
    for attempt in 0..5 {
        info!("GET {} (attempt {} of 5)", url, attempt + 1);
        let resp = client.get(url).send();
        match resp {
            Ok(r) if r.status().is_success() => {
                info!("<- {}", r.status());
                debug!("<- headers: {:?}", r.headers());
                return Ok(r)
            },
            Ok(r) if matches!(r.status(), StatusCode::TOO_MANY_REQUESTS | StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT | StatusCode::INTERNAL_SERVER_ERROR) => {
                warn!("<- {} (retryable)", r.status());
                if attempt == 4 { return Ok(r); }
                if let Some(retry_after) = r.headers().get(reqwest::header::RETRY_AFTER).and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<u64>().ok()) {
                    thread::sleep(Duration::from_secs(retry_after));
                } else {
                    thread::sleep(delay);
                    delay *= 2;
                }
                continue;
            }
            Ok(r) => {
                warn!("<- {} (non-retryable)", r.status());
                return Ok(r);
            }
            Err(e) => {
                warn!("transport error: {}", e);
                if attempt == 4 { return Err(e).context("request error") }
                thread::sleep(delay);
                delay *= 2;
            }
        }
    }
    unreachable!();
}

/// Build the ENA search URL for an arbitrary query + field list.
fn build_url(query: &str, fields: &str) -> String {
    let enc_query = utf8_percent_encode(query, NON_ALPHANUMERIC).to_string();
    let url = format!(
        "{base}/search?result=read_run&dataPortal=ena&query={query}&fields={fields}&format=json&limit=0",
        base = PORTAL_BASE,
        query = enc_query,
        fields = fields
    );
    debug!("built URL: {}", url);
    url
}

/// Lightweight health check of ENA endpoints used by this client.
fn ping_results(client: &Client) -> Result<()> {
    let url = format!("{}/results?dataPortal=ena", PORTAL_BASE);
    let r = request_with_retries(client, &url)?;
    if r.status().is_success() { Ok(()) } else { bail!("results ping failed: {}", r.status()) }
}

/// Perform a minimal handshake to surface early connectivity / rate limit issues.
fn handshake(client: &Client) -> Result<()> {
    if let Err(e) = ping_results(client) {
        warn!("ENA results ping failed: {}", e);
    }
    let raw_q: &str = r#"instrument_platform="OXFORD_NANOPORE""#;
    debug!("handshake raw_query: {}", raw_q);
    let url2 = build_url(raw_q, "run_accession").replace("limit=0", "limit=1");
    let r2 = request_with_retries(client, &url2)?;
    if !r2.status().is_success() {
        warn!("handshake minimal search failed: {}", r2.status());
    }
    Ok(())
}

/// Fetch runs within a **rolling** window: `first_public >= since` **OR** `last_updated >= since`.
pub fn fetch_runs_since(since: chrono::NaiveDate) -> Result<Vec<RunRecord>> {
    let ua = "herring/0.2.1 (+https://nanoporetech.com)";
    let client = make_client(ua)?;

    if let Err(e) = handshake(&client) {
        warn!("ENA handshake warning: {}", e);
    }

    let fields = [
        "run_accession",
        "study_accession",
        "sample_accession",
        "base_count",
        "instrument_model",
        "library_strategy",
        "scientific_name",
        "first_public",
        "study_title",
    ].join(",");

    let q_full = format!(
        r#"instrument_platform="OXFORD_NANOPORE" AND (first_public>={d} OR last_updated>={d})"#,
        d = since.format("%Y-%m-%d")
    );
    debug!("full-window raw_query: {}", q_full);
    let url_full = build_url(&q_full, &fields);
    let resp = request_with_retries(&client, &url_full)?;
    if resp.status().is_success() {
        let runs: Vec<RunRecord> = resp.json().context("decode read_run json")?;
        info!("fetched {} runs in full-window request", runs.len());
        return Ok(runs);
    }

    let today = chrono::Utc::now().date_naive();
    let mut dedup: HashSet<String> = HashSet::new();
    let mut out: Vec<RunRecord> = Vec::new();

    let mut start = since;
    while start <= today {
        let end = std::cmp::min(start + chrono::Duration::days(13), today);
        let q = format!(
            r#"instrument_platform="OXFORD_NANOPORE" AND ((first_public>={s} AND first_public<={e}) OR (last_updated>={s} AND last_updated<={e}))"#,
            s = start.format("%Y-%m-%d"),
            e = end.format("%Y-%m-%d")
        );
        debug!("window raw_query: {}", q);
        let url = build_url(&q, &fields);
        let r = request_with_retries(&client, &url)?;
        if !r.status().is_success() { bail!("ENA search(read_run) failed: {} (window {}..{})", r.status(), start, end); }
        let mut runs: Vec<RunRecord> = r.json().context("decode read_run json (windowed)")?;
        let before = out.len();
        for rec in runs.drain(..) {
            if let Some(acc) = rec.run_accession.as_ref() {
                if dedup.insert(acc.clone()) { out.push(rec); }
            } else {
                out.push(rec);
            }
        }
        info!("window {}..{} -> {} new runs ({} total)", start, end, out.len() - before, out.len());
        start = end + chrono::Duration::days(1);
    }

    Ok(out)
}

/// Fetch runs within a **fixed release** window: `first_public ∈ [start, end]`.
pub fn fetch_runs_between(start: chrono::NaiveDate, end: chrono::NaiveDate) -> Result<Vec<RunRecord>> {
    let ua = "herring/0.2.1 (+https://nanoporetech.com)";
    let client = make_client(ua)?;
    if let Err(e) = handshake(&client) {
        warn!("ENA handshake warning: {}", e);
    }

    let fields = [
        "run_accession","study_accession","sample_accession","base_count",
        "instrument_model","library_strategy","scientific_name","first_public","study_title",
    ].join(",");

    let mut dedup: HashSet<String> = HashSet::new();
    let mut out: Vec<RunRecord> = Vec::new();

    let mut s = start;
    while s <= end {
        let e = std::cmp::min(s + chrono::Duration::days(13), end);
        let q = format!(
            r#"instrument_platform="OXFORD_NANOPORE" AND (first_public>={s} AND first_public<={e})"#,
            s = s.format("%Y-%m-%d"),
            e = e.format("%Y-%m-%d")
        );
        debug!("released-only window raw_query: {}", q);
        let url = build_url(&q, &fields);
        let r = request_with_retries(&client, &url)?;
        if !r.status().is_success() { bail!("ENA search(read_run) failed: {} (released window {}..{})", r.status(), s, e); }
        let mut runs: Vec<RunRecord> = r.json().context("decode read_run json (released window)")?;
        for rec in runs.drain(..) {
            if let Some(acc) = rec.run_accession.as_ref() {
                if dedup.insert(acc.clone()) { out.push(rec); }
            } else {
                out.push(rec);
            }
        }
        s = e + chrono::Duration::days(1);
    }

    info!("released-only window {}..{} -> {} runs", start, end, out.len());
    Ok(out)
}
