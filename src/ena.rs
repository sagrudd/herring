//! ENA integration layer for `herring`.
//
//! Resilient ENA Portal API calls with retries and windowing.
//
//! - `dataPortal=ena` explicitly set
//! - retry/backoff on 5xx/429
//! - fallback to 14-day windows, dedupe by `run_accession`
use anyhow::{bail, Context, Result};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{blocking::Client, Certificate, StatusCode};
use serde::Deserialize;
use std::{collections::HashSet, env, fs, thread, time::Duration};

const PORTAL_BASE: &str = "https://www.ebi.ac.uk/ena/portal/api";

#[derive(Debug, Deserialize, Clone)]
pub struct RunRecord {
    pub run_accession: Option<String>,
    pub study_accession: String,
    pub instrument_model: Option<String>,
    pub library_strategy: Option<String>,
    pub scientific_name: Option<String>,
    pub first_public: Option<String>,
    pub study_title: Option<String>,
    pub sample_accession: Option<String>,
    pub base_count: Option<String>,
    pub fastq_bytes: Option<String>,
    pub submitted_bytes: Option<String>,
}

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

fn make_client(ua: &str) -> Result<Client> {
    let mut builder = Client::builder().user_agent(ua);
    if env::var("HERRING_INSECURE_TLS").as_deref() == Ok("1") {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Ok(p) = env::var("HERRING_CA_BUNDLE") {
        let pem = fs::read(p)?;
        builder = builder.add_root_certificate(Certificate::from_pem(&pem)?);
    }
    let timeout = env::var("HERRING_TIMEOUT_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(30);
    builder = builder.timeout(Duration::from_secs(timeout));
    Ok(builder.build()?)
}

fn request_with_retries(client: &Client, url: &str) -> Result<reqwest::blocking::Response> {
    let mut delay = Duration::from_millis(400);
    for attempt in 0..5 {
        let resp = client.get(url).send();
        match resp {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) if matches!(r.status(), StatusCode::TOO_MANY_REQUESTS | StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT | StatusCode::INTERNAL_SERVER_ERROR) => {
                if attempt == 4 { return Ok(r); }
                if let Some(retry_after) = r.headers().get(reqwest::header::RETRY_AFTER).and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<u64>().ok()) {
                    thread::sleep(Duration::from_secs(retry_after));
                } else {
                    thread::sleep(delay);
                    delay *= 2;
                }
                continue;
            }
            Ok(r) => return Ok(r),
            Err(e) => {
                if attempt == 4 { return Err(e).context("request error") }
                thread::sleep(delay);
                delay *= 2;
            }
        }
    }
    unreachable!();
}

fn build_url(query: &str, fields: &str) -> String {
    format!(
        "{base}/search?result=read_run&dataPortal=ena&query={query}&fields={fields}&format=json&limit=0",
        base = PORTAL_BASE,
        query = utf8_percent_encode(query, NON_ALPHANUMERIC),
        fields = fields
    )
}

pub fn fetch_runs_since(since: chrono::NaiveDate) -> Result<Vec<RunRecord>> {
    let ua = "herring/0.1.22 (+https://nanoporetech.com)";
    let client = make_client(ua)?;

    let fields = [
        "run_accession",
        "study_accession",
        "instrument_model",
        "library_strategy",
        "scientific_name",
        "first_public",
        "study_title",
        "sample_accession",
        "base_count",
        "fastq_bytes",
        "submitted_bytes",
    ].join(",");

    // 1) Full-window attempt
    let q_full = format!(
        "instrument_platform=\\\"OXFORD_NANOPORE\\\" AND (first_public>={d} OR last_updated>={d})",
        d = since.format("%Y-%m-%d")
    );
    let url_full = build_url(&q_full, &fields);
    let resp = request_with_retries(&client, &url_full)?;
    if resp.status().is_success() {
        let runs: Vec<RunRecord> = resp.json().context("decode read_run json")?;
        return Ok(runs);
    }

    // 2) Windowed fallback
    let today = chrono::Utc::now().date_naive();
    let mut dedup: HashSet<String> = HashSet::new();
    let mut out: Vec<RunRecord> = Vec::new();

    let mut start = since;
    while start <= today {
        let end = std::cmp::min(start + chrono::Duration::days(13), today);
        let q = format!(
            "instrument_platform=\\\"OXFORD_NANOPORE\\\" AND ((first_public>={s} AND first_public<={e}) OR (last_updated>={s} AND last_updated<={e}))",
            s = start.format("%Y-%m-%d"),
            e = end.format("%Y-%m-%d")
        );
        let url = build_url(&q, &fields);
        let r = request_with_retries(&client, &url)?;
        if !r.status().is_success() { bail!("ENA search(read_run) failed: {} (window {}..{})", r.status(), start, end); }
        let mut runs: Vec<RunRecord> = r.json().context("decode read_run json (windowed)")?;
        for rec in runs.drain(..) {
            if let Some(acc) = rec.run_accession.as_ref() {
                if dedup.insert(acc.clone()) { out.push(rec); }
            } else {
                out.push(rec);
            }
        }
        start = end + chrono::Duration::days(1);
    }

    Ok(out)
}
