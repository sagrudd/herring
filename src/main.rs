use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use chrono::{Duration, Utc};
use polars::prelude::*;
use polars::prelude::SortMultipleOptions;

mod ena;
use ena::{fetch_runs_since, fetch_studies_by_accessions, map_platform, map_strategy, RunRecord, StudyRecord};

#[derive(Parser, Debug)]
#[command(name = "herring", version, about = "List recent ENA studies with Oxford Nanopore data")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List studies released or updated in the last N weeks (default: 8)
    List {
        /// Weeks back from today
        #[arg(short, long, default_value_t = 8)]
        weeks: i64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::List { weeks } => list_studies(weeks)?,
    }
    Ok(())
}

fn list_studies(weeks: i64) -> Result<()> {
    let since = (Utc::now() - Duration::weeks(weeks)).date_naive();

    // 1) Find ONT runs updated or released since `since`
    let runs: Vec<RunRecord> = fetch_runs_since(since)
        .context("fetching ONT runs from ENA portal API")?;

    if runs.is_empty() {
        println!("No Oxford Nanopore runs found in the last {} weeks.", weeks);
        return Ok(());
    }

    // Group info by study accession
    use std::collections::{BTreeMap, BTreeSet};
    let mut by_study: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    let mut fallback_release: BTreeMap<String, String> = BTreeMap::new();

    for r in &runs {
        let entry = by_study
            .entry(r.study_accession.clone())
            .or_insert_with(|| (BTreeSet::new(), BTreeSet::new(), BTreeSet::new()));
        entry.0.insert(map_platform(r.instrument_model.as_deref()).to_string());
        if let Some(strat) = r.library_strategy.as_deref() {
            entry.1.insert(map_strategy(strat));
        }
        if let Some(sp) = r.scientific_name.as_deref() {
            if !sp.is_empty() { entry.2.insert(sp.to_string()); }
        }
        // Keep earliest run-level first_public as fallback
        if let Some(fp) = r.first_public.as_deref() {
            fallback_release
                .entry(r.study_accession.clone())
                .and_modify(|d| if &fp[..] < &d[..] { *d = fp.to_string(); })
                .or_insert_with(|| fp.to_string());
        }
    }

    let study_ids: Vec<String> = by_study.keys().cloned().collect();

    // 2) Fetch study-level metadata
    let studies: Vec<StudyRecord> = fetch_studies_by_accessions(&study_ids)
        .context("fetching study records from ENA portal API")?;

    // 3) Build rows
    #[derive(Clone)]
    struct Row { acc: String, release: String, platform: String, seq_type: String, species: String, title: String }

    let mut rows: Vec<Row> = Vec::new();
    use std::collections::HashMap;
    let study_map: HashMap<String, &StudyRecord> = studies.iter().map(|s| (s.study_accession.clone(), s)).collect();

    for (acc, (plats, types, species)) in by_study {
        let plat = join_set(plats);
        let seqt = join_set(types);
        let sp = join_set_max(species, 5);
        let (release, title) = if let Some(s) = study_map.get(&acc) {
            (s.first_public.clone().unwrap_or_else(|| fallback_release.get(&acc).cloned().unwrap_or_default()),
             s.study_title.clone().unwrap_or_default())
        } else {
            (fallback_release.get(&acc).cloned().unwrap_or_default(), String::new())
        };
        let seq_type = if !seqt.is_empty() { seqt } else { study_map.get(&acc).and_then(|s| s.study_type.clone()).unwrap_or_default() };
        rows.push(Row { acc, release, platform: plat, seq_type, species: sp, title });
    }

    // Sort rows by date descending (YYYY-MM-DD lexicographic works)
    rows.sort_by(|a, b| b.release.cmp(&a.release));

    // 4) To Polars DataFrame
    let acc: Vec<_> = rows.iter().map(|r| r.acc.as_str()).collect();
    let release: Vec<_> = rows.iter().map(|r| r.release.as_str()).collect();
    let platform: Vec<_> = rows.iter().map(|r| r.platform.as_str()).collect();
    let seq_type: Vec<_> = rows.iter().map(|r| r.seq_type.as_str()).collect();
    let species: Vec<_> = rows.iter().map(|r| r.species.as_str()).collect();
    let title: Vec<_> = rows.iter().map(|r| r.title.as_str()).collect();

    let df = df!(
        "study_accession" => acc,
        "release_date" => release,
        "platform" => platform,
        "sequencing_type" => seq_type,
        "species" => species,
        "study_title" => title,
    )?;

    let df = df.sort(
        ["release_date"],
        SortMultipleOptions { descending: vec![true], ..Default::default() }
    )?; // newest first

    // Minimal, dependency-free table printer
    print_df(&df)?;

    Ok(())
}

fn join_set(set: std::collections::BTreeSet<String>) -> String {
    set.into_iter().collect::<Vec<_>>().join(", ")
}
fn join_set_max(set: std::collections::BTreeSet<String>, max: usize) -> String {
    let mut v: Vec<_> = set.into_iter().collect();
    if v.len() > max { v.truncate(max); }
    v.join(", ")
}

// Simple left-pad to width by appending spaces
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(width);
        out.push_str(s);
        out.push_str(&" ".repeat(width - len));
        out
    }
}

fn print_df(df: &DataFrame) -> Result<()> {
    let cols = df.get_columns();
    // Convert column names to owned Strings
    let names: Vec<String> = df.get_column_names()
        .iter()
        .map(|n| n.as_str().to_string())
        .collect();
    let nrows = df.height();

    // Compute column widths (use Debug formatting for AnyValue)
    let mut widths: Vec<usize> = names.iter().map(|n| n.chars().count()).collect();
    for (i, s) in cols.iter().enumerate() {
        for r in 0..nrows {
            let text = match s.get(r) {
                Ok(v) => format!("{v:?}"),
                Err(_) => "<err>".to_string(),
            };
            let len = text.chars().count();
            if len > widths[i] { widths[i] = len; }
        }
    }

    // Header
    let header = names.iter().enumerate()
        .map(|(i, n)| pad(n, widths[i]))
        .collect::<Vec<_>>()
        .join(" | ");
    println!("{}", header);

    // Separator
    let sep = widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("-+-");
    println!("{}", sep);

    // Rows
    for r in 0..nrows {
        let row = cols.iter().enumerate().map(|(i, s)| {
            let text = match s.get(r) {
                Ok(v) => format!("{v:?}"),
                Err(_) => "<err>".to_string(),
            };
            pad(&text, widths[i])
        }).collect::<Vec<_>>().join(" | ");
        println!("{}", row);
    }

    Ok(())
}