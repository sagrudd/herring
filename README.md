# herring 🐟 — ENA study lister for Oxford Nanopore data

**v0.1.32**
- FIX: unterminated string in `ena.rs` log line.
- Columns for **biosamples** (distinct `sample_accession`) and **gigabases** (sum of `base_count` / 1e9).
- Resilient handshake: ping `/results`, then a 1-record `search` (warn-only on failure).
- Correct quoting in queries → `%22` (no `%5C%22`).

Run:
```bash
cargo build --release
./target/release/herring list -vv
```
