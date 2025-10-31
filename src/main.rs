#![doc = include_str!("../README.md")]
#![deny(unsafe_code)]
#![warn(missing_docs)]

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use chrono::{Duration, Utc, NaiveDate};
use polars::prelude::*;
use polars::prelude::SortMultipleOptions;
use log::info;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use serde::Serialize;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

mod ena;
use ena::{fetch_runs_since, fetch_runs_between, map_platform, map_strategy, RunRecord};

#[derive(Parser, Debug)]
#[command(name = "herring", version, about = "List recent ENA studies with Oxford Nanopore data")]
/// Command-line interface definition.
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
/// Top-level commands for `herring`.
enum Commands {
    /// List studies released or updated in the last N weeks (default: 8).
    List {
        /// Weeks back from today (UTC) OR used as the window length with --from.
        #[arg(short, long, default_value_t = 8)]
        weeks: i64,
        /// Start date (YYYY-MM-DD) for a fixed release window. Uses first_public between FROM and FROM+weeks.
        #[arg(long, value_name="YYYY-MM-DD")]
        from: Option<String>,
        /// End date (YYYY-MM-DD) for a fixed release window; requires --from. Inclusive.
        #[arg(long, value_name="YYYY-MM-DD")]
        to: Option<String>,
        /// Increase log verbosity: -v (info), -vv (debug)
        #[arg(short, long, action = ArgAction::Count)]
        verbose: u8,
        /// Write CSV to path
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Write JSON to path
        #[arg(long)]
        json: Option<PathBuf>,
        /// Write HTML to path (sortable table)
        #[arg(long)]
        html: Option<PathBuf>,
    },
}

/// Initialize env_logger with a default filter from verbosity flags.
fn init_logger(verbosity: u8) {
    use env_logger::Env;
    let level = match verbosity { 0 => "warn", 1 => "info", _ => "debug" };
    let env = Env::default().default_filter_or(level);
    let mut b = env_logger::Builder::from_env(env);
    b.format_timestamp_secs();
    let _ = b.try_init();
}

/// Entry point.
fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::List { weeks, from, to, verbose, csv, json, html } => {
            init_logger(verbose);
            list_studies(weeks, from, to, csv, json, html)?
        }
    }
    Ok(())
}

#[derive(Serialize, Clone)]
/// JSON row shape used by `--json` export.
struct OutRow<'a> {
    study_accession: &'a str,
    release_date: &'a str,
    platform: &'a str,
    sequencing_type: &'a str,
    species: &'a str,
    biosamples: u32,
    gigabases: f64,
    study_title: &'a str,
}

#[derive(Clone)]
/// Internal aggregation row used for building tables/exports.
struct Row {
    acc: String,
    release: String,
    platform: String,
    seq_type: String,
    species: String,
    biosamples: u32,
    gigabases_num: f64,
    gigabases_str: String,
    title: String,
}

/// Execute the listing workflow and print/export results.
fn list_studies(weeks: i64, from: Option<String>, to: Option<String>, csv: Option<PathBuf>, json: Option<PathBuf>, html: Option<PathBuf>) -> Result<()> {
    let runs: Vec<RunRecord> = if let Some(from_s) = from {
        let start = NaiveDate::parse_from_str(&from_s, "%Y-%m-%d")
            .with_context(|| format!("--from must be YYYY-MM-DD, got: {}", from_s))?;
        let end_inclusive = if let Some(to_s) = to {
            let to_d = NaiveDate::parse_from_str(&to_s, "%Y-%m-%d")
                .with_context(|| format!("--to must be YYYY-MM-DD, got: {}", to_s))?;
            if to_d < start { bail!("--to ({}) is before --from ({})", to_d, start); }
            to_d
        } else {
            (start + Duration::weeks(weeks)) - Duration::days(1)
        };
        info!("released-only window: {} .. {} (inclusive)", start, end_inclusive);
        fetch_runs_between(start, end_inclusive)?
    } else {
        if to.is_some() { bail!("--to requires --from"); }
        let since = (Utc::now() - Duration::weeks(weeks)).date_naive();
        info!("rolling window (released OR updated) since {} ({} weeks)", since, weeks);
        fetch_runs_since(since)?
    };

    if runs.is_empty() {
        println!("No Oxford Nanopore runs found for the selected window.");
        return Ok(())
    }

    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    struct Agg {
        plats: BTreeSet<String>,
        types: BTreeSet<String>,
        species: BTreeSet<String>,
        samples: BTreeSet<String>,
        bases: u128,
        title: String,
        release: String,
    }

    let mut by_study: BTreeMap<String, Agg> = BTreeMap::new();

    for r in &runs {
        let a = by_study.entry(r.study_accession.clone()).or_default();
        a.plats.insert(map_platform(r.instrument_model.as_deref()).to_string());
        if let Some(strat) = r.library_strategy.as_deref() { a.types.insert(map_strategy(strat)); }
        if let Some(sp) = r.scientific_name.as_deref() { if !sp.is_empty() { a.species.insert(sp.to_string()); } }
        if let Some(fp) = r.first_public.as_deref() { if a.release.is_empty() || fp < a.release.as_str() { a.release = fp.to_string(); } }
        if let Some(t) = r.study_title.as_deref() { if !t.is_empty() && a.title.is_empty() { a.title = t.to_string(); } }
        if let Some(samp) = r.sample_accession.as_deref() { if !samp.is_empty() { a.samples.insert(samp.to_string()); } }
        if let Some(bc) = r.base_count.as_deref() {
            if let Ok(v) = bc.parse::<u64>() {
                a.bases = a.bases.saturating_add(v as u128);
            }
        }
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
        let biosamples = a.samples.len() as u32;
        let gb = (a.bases as f64) / 1e9_f64;
        let gigabases_num = (gb * 10.0).round() / 10.0; // one decimal
        let gigabases_str = format!("{:.1}", gigabases_num);
        rows.push(Row { acc, release: a.release, platform: plat, seq_type: seqt, species: sp, biosamples, gigabases_num, gigabases_str, title: a.title });
    }

    // DataFrame for stdout (gigabases as formatted string)
    let acc: Vec<_> = rows.iter().map(|r| r.acc.as_str()).collect();
    let release: Vec<_> = rows.iter().map(|r| r.release.as_str()).collect();
    let platform: Vec<_> = rows.iter().map(|r| r.platform.as_str()).collect();
    let seq_type: Vec<_> = rows.iter().map(|r| r.seq_type.as_str()).collect();
    let species: Vec<_> = rows.iter().map(|r| r.species.as_str()).collect();
    let biosamples: Vec<u32> = rows.iter().map(|r| r.biosamples).collect();
    let gigabases: Vec<_> = rows.iter().map(|r| r.gigabases_str.as_str()).collect();
    let title: Vec<_> = rows.iter().map(|r| r.title.as_str()).collect();

    let df = df!(
        "study_accession" => acc,
        "release_date" => release,
        "platform" => platform,
        "sequencing_type" => seq_type,
        "species" => species,
        "biosamples" => biosamples,
        "gigabases" => gigabases,
        "study_title" => title,
    )?;

    let df = df.sort(["release_date"], SortMultipleOptions { descending: vec![true], ..Default::default() })?;

    print_df(&df)?;

    if let Some(path) = csv { write_csv(&rows, path)?; }
    if let Some(path) = json { write_json(&rows, path)?; }
    if let Some(path) = html { write_html(&rows, path)?; }

    Ok(())
}

/// Write CSV export with human-formatted `gigabases`.
fn write_csv(rows: &[Row], path: PathBuf) -> Result<()> {
    let mut wtr = csv::Writer::from_path(&path)?;
    wtr.write_record([
        "study_accession","release_date","platform","sequencing_type","species","biosamples","gigabases","study_title"
    ])?;
    for r in rows {
        wtr.write_record([
            &r.acc, &r.release, &r.platform, &r.seq_type, &r.species,
            &r.biosamples.to_string(), &r.gigabases_str, &r.title
        ])?;
    }
    wtr.flush()?;
    println!("Wrote CSV to {}", path.display());
    Ok(())
}

/// Write JSON export (machine-friendly, numeric `gigabases`).
fn write_json(rows: &[Row], path: PathBuf) -> Result<()> {
    let out: Vec<OutRow> = rows.iter().map(|r| OutRow {
        study_accession: &r.acc,
        release_date: &r.release,
        platform: &r.platform,
        sequencing_type: &r.seq_type,
        species: &r.species,
        biosamples: r.biosamples,
        gigabases: r.gigabases_num,
        study_title: &r.title,
    }).collect();
    let f = File::create(&path)?;
    serde_json::to_writer_pretty(f, &out)?;
    println!("Wrote JSON to {}", path.display());
    Ok(())
}

/// Minimal HTML escaping.
fn escape_html(s: &str) -> String {

    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\"', "&quot;").replace('\'', "&#39;")
}

fn wikipedia_search_url(title: &str) -> String {
    let enc = utf8_percent_encode(title.trim(), NON_ALPHANUMERIC).to_string();
    format!("https://en.wikipedia.org/w/index.php?search={}", enc)
}

/// Write a sortable HTML table; ENA accessions + species Wikipedia search links.
fn write_html(rows: &[Row], path: PathBuf) -> Result<()> {
    let mut f = File::create(&path)?;
    let mut html = String::new();
    html.push_str("<!doctype html><meta charset=\"utf-8\"><title>herring results</title>\n");
    html.push_str("<style>body{font:14px system-ui, sans-serif;padding:16px} table{border-collapse:collapse;width:100%} th,td{border:1px solid #ddd;padding:6px 8px} th{cursor:pointer;background:#f6f6f6;position:sticky;top:0} tr:nth-child(even){background:#fafafa} a{color:#0645ad;text-decoration:none}</style>\n");
    html.push_str("<h1>herring results</h1>\n");
    html.push_str("<p>Click a column header to sort. Default sort is by date (newest first).</p>\n");
    html.push_str("<table id=\"t\"><thead><tr>\n");
    let headers = [
        ("study_accession","str"),("release_date","date"),("platform","str"),("sequencing_type","str"),("species","str"),("biosamples","num"),("gigabases","num"),("study_title","str")
    ];
    for (h, ty) in headers.iter() {
        html.push_str(&format!("<th data-type=\"{}\">{}</th>", ty, h.replace('_'," ")));
    }
    html.push_str("</tr></thead><tbody>\n");
    for r in rows {
        let url = format!("https://www.ebi.ac.uk/ena/browser/view/{}", r.acc);
        html.push_str("<tr>");
        html.push_str(&format!("<td><a href=\"{}\" target=\"_blank\" rel=\"noopener\">{}</a></td>", url, escape_html(&r.acc)));
        html.push_str(&format!("<td>{}</td>", escape_html(&r.release)));
        html.push_str(&format!("<td>{}</td>", escape_html(&r.platform)));
        html.push_str(&format!("<td>{}</td>", escape_html(&r.seq_type)));
        let species_links = if r.species.trim().is_empty() { String::new() } else { r.species.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener\">{}</a>", wikipedia_search_url(s), escape_html(s))).collect::<Vec<_>>().join(", ") };
        html.push_str(&format!("<td>{}</td>", species_links));
        html.push_str(&format!("<td data-v=\"{}\">{}</td>", r.biosamples, r.biosamples));
        html.push_str(&format!("<td data-v=\"{}\">{}</td>", r.gigabases_num, r.gigabases_str));
        html.push_str(&format!("<td>{}</td>", escape_html(&r.title)));
        html.push_str("</tr>\n");
    }
    html.push_str("</tbody></table>\n");
    html.push_str(r#"<script>
(function(){
  const tbl=document.getElementById('t');
  const get=(cell)=>{
    const td=cell;
    const v=td.getAttribute('data-v');
    if(v!==null) return parseFloat(v);
    return td.textContent.trim();
  };
  const cmp=(a,b,ty)=>{
    if(ty==='num') return a-b;
    if(ty==='date') return (a>b)-(a<b);
    return a.localeCompare(b);
  };
  tbl.querySelectorAll('th').forEach((th,i)=>{
    let asc=false;
    th.addEventListener('click',()=>{
      const ty=th.getAttribute('data-type');
      const rows=[...tbl.tBodies[0].rows];
      rows.sort((r1,r2)=>{
        const a=get(r1.cells[i]);
        const b=get(r2.cells[i]);
        return (asc?1:-1)*cmp(a,b,ty);
      });
      asc=!asc;
      rows.forEach(r=>tbl.tBodies[0].appendChild(r));
    });
  });
})();
</script>"#);

    f.write_all(html.as_bytes())?;
    println!("Wrote HTML to {}", path.display());
    Ok(())
}

/// Right-pad with spaces to width, measured in `chars()`.
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width { s.to_string() } else { format!("{s}{:>width$}", "", width = width - len) }
}

/// Print a simple monospace table to stdout.
fn print_df(df: &DataFrame) -> Result<()> {
    let cols = df.get_columns();
    let names: Vec<String> = df.get_column_names_owned().into_iter().map(|n| n.to_string()).collect();
    let nrows = df.height();

    fn cell_as_str(s: &Series, r: usize) -> String {
        match s.get(r) {
            Ok(AnyValue::Null) => "".to_string(),
            Ok(v) => v.to_string(),
            Err(_) => "".to_string(),
        }
    }

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
