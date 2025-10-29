#![doc = include_str!("../README.md")]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use chrono::{Duration, Utc};
use polars::prelude::*;
use polars::prelude::SortMultipleOptions;

mod ena;
use ena::{fetch_runs_since, map_platform, map_strategy, RunRecord};

/// Top-level CLI for the `herring` tool.
#[derive(Parser, Debug)]
#[command(name = "herring", version, about = "List recent ENA studies with Oxford Nanopore data")]
struct Cli {
    /// Subcommands (currently only `list`).
    #[command(subcommand)]
    command: Commands,
}

/// CLI subcommands.
#[derive(Subcommand, Debug)]
enum Commands {
    /// List studies released or updated in the last N weeks (default: 8).
    ///
    /// The listing aggregates run-level records by `study_accession`.
    /// See the README for column definitions.
    List {
        /// Weeks back from today (UTC).
        #[arg(short, long, default_value_t = 8)]
        weeks: i64,
    },
}

/// Program entrypoint.
fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::List { weeks } => list_studies(weeks)?,
    }
    Ok(())
}

/// List ENA studies with Oxford Nanopore sequencing data released/updated in the
/// last `weeks` weeks, aggregated at the study level and rendered as a table.
///
/// # Notes
/// - Uses the ENA Portal API and requests run-level fields sufficient to build
///   a study-level summary without a second query.
/// - Printing is intentionally dependency-free (no fancy table crate) to keep
///   MSRV low and builds fast.
fn list_studies(weeks: i64) -> Result<()> {
    let since = (Utc::now() - Duration::weeks(weeks)).date_naive();

    let runs: Vec<RunRecord> = fetch_runs_since(since)
        .context("fetching ONT runs from ENA portal API")?;

    if runs.is_empty() {
        println!("No Oxford Nanopore runs found in the last {} weeks.", weeks);
        return Ok(());
    }

    use std::collections::{BTreeMap, BTreeSet};

    /// Aggregated, per-study values accumulated from run-level rows.
    #[derive(Default)]
    struct Agg {
        plats: BTreeSet<String>,
        types: BTreeSet<String>,
        species: BTreeSet<String>,
        samples: BTreeSet<String>,
        title: String,
        release: String,
        base_bp: u128,
        bytes: u128,
    }

    let mut by_study: BTreeMap<String, Agg> = BTreeMap::new();

    for r in &runs {
        let a = by_study.entry(r.study_accession.clone()).or_default();
        a.plats.insert(map_platform(r.instrument_model.as_deref()).to_string());
        if let Some(strat) = r.library_strategy.as_deref() { a.types.insert(map_strategy(strat)); }
        if let Some(sp) = r.scientific_name.as_deref() { if !sp.is_empty() { a.species.insert(sp.to_string()); } }
        if let Some(samp) = r.sample_accession.as_deref() { if !samp.is_empty() { a.samples.insert(samp.to_string()); } }
        if let Some(fp) = r.first_public.as_deref() { if a.release.is_empty() || fp < a.release.as_str() { a.release = fp.to_string(); } }
        if let Some(t) = r.study_title.as_deref() { if !t.is_empty() && a.title.is_empty() { a.title = t.to_string(); } }
        if let Some(bc) = r.base_count.as_deref().and_then(|v| v.parse::<u128>().ok()) { a.base_bp += bc; }
        let bytes = r
            .fastq_bytes.as_deref().and_then(|v| v.parse::<u128>().ok())
            .or_else(|| r.submitted_bytes.as_deref().and_then(|v| v.parse::<u128>().ok()))
            .unwrap_or(0);
        a.bytes += bytes;
    }

    /// A single row destined for the Polars DataFrame.
    #[derive(Clone)]
    struct Row {
        acc: String,
        release: String,
        platform: String,
        seq_type: String,
        species: String,
        title: String,
        biosamples: u32,
        gbases: f64,
        gbytes: f64,
    }
    let mut rows: Vec<Row> = Vec::new();

    for (acc, a) in by_study.into_iter() {
        let plat = a.plats.into_iter().collect::<Vec<_>>().join(", ");
        let seqt = a.types.into_iter().collect::<Vec<_>>().join(", ");
        let sp = {
            let mut v: Vec<_> = a.species.into_iter().collect();
            if v.len() > 5 { v.truncate(5); }
            v.join(", ")
        };
        let gbases = (a.base_bp as f64) / 1e9_f64;
        let gbytes = (a.bytes as f64) / 1e9_f64;
        rows.push(Row { acc, release: a.release, platform: plat, seq_type: seqt, species: sp, title: a.title, biosamples: a.samples.len() as u32, gbases, gbytes });
    }

    // Build Polars DataFrame
    let acc: Vec<_> = rows.iter().map(|r| r.acc.as_str()).collect();
    let release: Vec<_> = rows.iter().map(|r| r.release.as_str()).collect();
    let platform: Vec<_> = rows.iter().map(|r| r.platform.as_str()).collect();
    let seq_type: Vec<_> = rows.iter().map(|r| r.seq_type.as_str()).collect();
    let species: Vec<_> = rows.iter().map(|r| r.species.as_str()).collect();
    let title: Vec<_> = rows.iter().map(|r| r.title.as_str()).collect();
    let biosamples: Vec<_> = rows.iter().map(|r| r.biosamples).collect();
    let gbases: Vec<_> = rows.iter().map(|r| r.gbases).collect();
    let gbytes: Vec<_> = rows.iter().map(|r| r.gbytes).collect();

    let df = df!(
        "study_accession" => acc,
        "release_date" => release,
        "platform" => platform,
        "sequencing_type" => seq_type,
        "species" => species,
        "study_title" => title,
        "biosamples" => biosamples,
        "gbases" => gbases,
        "gbytes" => gbytes,
    )?;

    let df = df.sort(["release_date"], SortMultipleOptions { descending: vec![true], ..Default::default() })?;

    print_df(&df)?;
    Ok(())
}

/// Pad a string on the right with spaces to a given width.
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width { s.to_string() } else { format!("{s}{:>width$}", "", width = width - len) }
}

/// Print a `polars::DataFrame` as a simple, width-aligned table without
/// introducing extra dependencies.
fn print_df(df: &DataFrame) -> Result<()> {
    let cols = df.get_columns();
    let names: Vec<String> = df.get_column_names_owned().into_iter().map(|n| n.to_string()).collect();
    let nrows = df.height();

    /// Project a cell to a printable string; `Null` becomes empty string.
    fn cell_as_str(s: &Series, r: usize) -> String {
        match s.get(r) {
            Ok(AnyValue::Null) => "".to_string(),
            Ok(v) => v.to_string(),
            Err(_) => "".to_string(),
        }
    }

    // Compute column widths
    let mut widths: Vec<usize> = names.iter().map(|n| n.chars().count()).collect();
    for (i, s) in cols.iter().enumerate() {
        for r in 0..nrows {
            let text = cell_as_str(s, r);
            let len = text.chars().count();
            if len > widths[i] { widths[i] = len; }
        }
    }

    let header = names.iter().enumerate().map(|(i, n)| pad(n, widths[i])).collect::<Vec<_>>().join(" | ");
    println!("{}", header);
    let sep = widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("-+-");
    println!("{}", sep);

    for r in 0..nrows {
        let row = cols.iter().enumerate().map(|(i, s)| {
            let text = cell_as_str(s, r);
            pad(&text, widths[i])
        }).collect::<Vec<_>>().join(" | ");
        println!("{}", row);
    }
    Ok(())
}
