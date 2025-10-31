# herring 🐟 — ENA study lister for Oxford Nanopore data

**v0.1.30**
- Restore full `main.rs` (no stubs).
- Keep resilient handshake: ping `/results`, then a 1-record `search`; failures warn and continue.
- Correct quoting: use literal quotes in queries so URLs contain `%22` (no `%5C%22`).

Run:
```bash
cargo build --release
./target/release/herring list -vv
```
