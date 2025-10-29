use anyhow::{bail, Context, Result};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{blocking::Client, Certificate};
use serde::Deserialize;
use std::{collections::HashSet, env, fs};

const PORTAL_BASE: &str = "https://www.ebi.ac.uk/ena/portal/api";

#[derive(Debug, Deserialize, Clone)]
pub struct RunRecord {
    pub study_accession: String,
    pub instrument_model: Option<String>,
    pub library_strategy: Option<String>,
    pub scientific_name: Option<String>,
    pub first_public: Option<String>,
    pub study_title: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StudyRecord {
    pub study_accession: String,
    pub first_public: Option<String>,
    pub last_updated: Option<String>,
    pub study_title: Option<String>,
    pub study_type: Option<String>,
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
    Ok(builder.build()?)
}

pub fn fetch_runs_since(since: chrono::NaiveDate) -> Result<Vec<RunRecord>> {
    let q = format!(
        "instrument_platform=\"OXFORD_NANOPORE\" AND (first_public>={d} OR last_updated>={d})",
        d = since.format("%Y-%m-%d")
    );
    let fields = [
        "study_accession",
        "instrument_model",
        "library_strategy",
        "scientific_name",
        "first_public",
        "study_title",
    ].join(",");

    let url = format!(
        "{base}/search?result=read_run&query={query}&fields={fields}&format=json&limit=0",
        base = PORTAL_BASE,
        query = utf8_percent_encode(&q, NON_ALPHANUMERIC),
        fields = fields
    );

    let client = make_client("herring/0.1.16 (+https://nanoporetech.com)")?;
    let resp = client.get(&url).send().context("request runs")?;
    if !resp.status().is_success() { bail!("ENA search(read_run) failed: {}", resp.status()); }
    let runs: Vec<RunRecord> = resp.json().context("decode read_run json")?;
    Ok(runs)
}

// Kept for reference but not used. Uses 'accession' with ORs and parentheses.
pub fn fetch_studies_by_accessions(accs: &[String]) -> Result<Vec<StudyRecord>> {
    if accs.is_empty() { return Ok(vec![]); }

    let client = make_client("herring/0.1.16")?;

    let mut out: Vec<StudyRecord> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for chunk in accs.chunks(100) {
        let ors = format!("({})", chunk.iter().map(|a| format!("accession=\"{}\"", a)).collect::<Vec<_>>().join(" OR "));
        let q = utf8_percent_encode(&ors, NON_ALPHANUMERIC);
        let fields = [
            "study_accession",
            "first_public",
            "last_updated",
            "study_title",
            "study_type",
        ].join(",");
        let url = format!(
            "{base}/search?result=study&query={query}&fields={fields}&format=json&limit=0",
            base = PORTAL_BASE,
            query = q,
            fields = fields
        );
        let resp = client.get(&url).send().context("request studies")?;
        if !resp.status().is_success() { bail!("ENA search(study) failed: {}", resp.status()); }
        let mut v: Vec<StudyRecord> = resp.json().context("decode study json")?;
        for s in v.drain(..) {
            if seen.insert(s.study_accession.clone()) {
                out.push(s);
            }
        }
    }
    Ok(out)
}