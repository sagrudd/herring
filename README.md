# herring

CLI to list ENA studies (projects) in the last N weeks that include Oxford Nanopore data, shown as a Polars table.

## Build
```bash
cd herring
cargo build --release
```

## Run
```bash
cargo run -- list --weeks 8
```

*Columns*: `study_accession`, `release_date`, `platform`, `sequencing_type`, `species`, `study_title`.