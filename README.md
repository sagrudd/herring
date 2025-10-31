# herring 🐟 — ENA study lister for Oxford Nanopore data

**Version:** 0.2.1 (minor release)

> CLI that finds ENA studies with Oxford Nanopore (ONT) sequencing runs and summarizes them in a table.  
> Supports rolling and fixed-date windows, CSV/JSON/HTML export, sortable HTML, and Wikipedia **search** links for species.

---

## ✨ What’s new in 0.2.0
- **Minor bump** from 0.1.x → **0.2.0**.
- **Deep rustdoc** across the crate with paranoid-level detail and examples.
- README refreshed end-to-end, including schema, examples, env vars, and caveats.
- Clear **attribution**: ChatGPT (OpenAI GPT-5 Thinking) assisted in authoring.
- Maintains strict **Rust 1.80** compatibility and avoids edition-2024 dependencies.

---

## 🚀 Quick start
```bash
cargo build --release

# Rolling: released OR updated within the last 8 weeks (default)
./target/release/herring list

# More logging
./target/release/herring list -vv

# Fixed release window: FROM .. FROM+weeks (released-only via `first_public`)
./target/release/herring list --from 2024-01-01 --weeks 12

# Export
./target/release/herring list --json out.json --csv out.csv --html out.html
```

---

## 🧭 Query semantics

### Modes
- **Rolling window (default):** `first_public >= (now - weeks)` **OR** `last_updated >= (now - weeks)`  
  Good for staying current with new or updated datasets.
- **Fixed release window:** `--from YYYY-MM-DD --weeks N`  
  Queries **released-only** datasets where `first_public` ∈ `[FROM, FROM + N weeks)`.
  The implementation uses inclusive daily windows with chunking to respect API behavior.

### Platform filter
All queries include: `instrument_platform="OXFORD_NANOPORE"`

### Fields requested
`run_accession, study_accession, sample_accession, base_count, instrument_model, library_strategy, scientific_name, first_public, study_title`

---

## 📊 Output columns
`study_accession | release_date | platform | sequencing_type | species | biosamples | gigabases | study_title`

- **release_date** — Earliest `first_public` among runs in the study (YYYY-MM-DD).
- **platform** — Inferred (PromethION / GridION / MinION) from instrument model.
- **sequencing_type** — From `library_strategy`; grouped to genome/transcriptome/metagenome when possible.
- **species** — Up to 5 unique names.
- **biosamples** — Count of unique `sample_accession` per study.
- **gigabases** — Sum of `base_count` / 1e9, rounded to **1 decimal** for readability.

---

## 🧪 JSON schema (Draft-07)
```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "HerringENAStudies",
  "type": "array",
  "items": {
    "type": "object",
    "required": [
      "study_accession","release_date","platform",
      "sequencing_type","species","biosamples","gigabases","study_title"
    ],
    "properties": {
      "study_accession": {"type": "string", "pattern": "^PRJ[EN][AB].+"},
      "release_date":    {"type": "string", "format": "date"},
      "platform":        {"type": "string"},
      "sequencing_type": {"type": "string"},
      "species":         {"type": "string"},
      "biosamples":      {"type": "integer", "minimum": 0},
      "gigabases":       {"type": "number",  "minimum": 0},
      "study_title":     {"type": "string"}
    },
    "additionalProperties": false
  }
}
```

---

## 🌐 HTML export
- Sortable columns (click headers).
- ENA accessions linked to ENA Browser.
- **Species** entries link to **Wikipedia search** (not direct article) for better reliability:
  `https://en.wikipedia.org/w/index.php?search=<species name>`
- Single self-contained file (no external assets).

---

## ⚙️ Flags
```
USAGE:
  herring list [OPTIONS]

OPTIONS:
  -w, --weeks <N>         Window length in weeks (default: 8). With --from, defines window size.
      --from YYYY-MM-DD   Fixed release window start date (inclusive). Uses first_public only.
  -v, --verbose           Increase log level (-v info, -vv debug)
      --csv <PATH>        Write CSV
      --json <PATH>       Write JSON (matches the schema above)
      --html <PATH>       Write HTML (sortable table)
  -h, --help              Print help
  -V, --version           Print version
```

---

## 🔐 TLS & networking
- Uses `reqwest` + `rustls-tls-native-roots` in **blocking** mode for Rust 1.80 compatibility.
- Environment variables:
  - `HERRING_INSECURE_TLS=1` — disable TLS validation (**only for debugging**).
  - `HERRING_CA_BUNDLE=/path/to/ca.pem` — add root CAs.
  - `HERRING_TIMEOUT_SECS=30` — request timeout in seconds.
- Gentle retries with exponential backoff on `5xx/429` and selected gateway errors.
- A lightweight handshake probes ENA availability and a 1-record test query.

---

## 🧱 Limits & caveats
- Aggregations are derived from **run-level** rows; studies without runs in the window are naturally excluded.
- Species are rendered as typed; no taxonomy normalization is attempted.
- `base_count` is assumed to be bases; conversion to GiB is not attempted in this release.
- The ENA API can occasionally return transient 500s; retries are applied.

---

## 🧩 Developing
```bash
# Build & run
cargo run -- list -vv --from 2024-01-01 --weeks 12

# Docs (includes crate-level README via rustdoc)
cargo doc --no-deps --open
```
Coding style:
- No `unsafe`.
- Clippy- and rustfmt-friendly (Rust 1.80).
- Minimal dependencies locked to 1.80-compatible versions.

---

## 👥 Attribution
This project was produced by **Stephen Rudd** with extensive assistance from **ChatGPT (OpenAI GPT-5 Thinking)** via prompt-driven pair programming.  
Please include this attribution in derivative works.

---

## 📜 License
Dual-licensed under **MIT** or **Apache-2.0** at your option.

---

## 🧭 Date windowing recap
- **Rolling (default):** `first_public >= now-weeks OR last_updated >= now-weeks`.
- **Fixed release window:** `--from YYYY-MM-DD [--to YYYY-MM-DD] [--weeks N]`
  - If `--to` is present: inclusive `[FROM, TO]`.
  - Else: `[FROM, FROM + N weeks)` (end-exclusive in concept; implemented as end-1 day per API semantics).

---

## 🤖 Continuous Integration
A GitHub Actions workflow runs on every push/PR:
- `cargo fmt --check`
- `cargo clippy -D warnings`
- `cargo build --locked`
- `cargo check --locked`
Target toolchain: Rust **1.80.0**.
