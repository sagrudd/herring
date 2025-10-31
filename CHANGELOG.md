# Changelog

All notable changes to this project will be documented in this file.

## [0.2.1] - 2025-10-31
### Added
- `--to YYYY-MM-DD` optional end date for fixed **release** windows. When used with `--from`, the window is inclusive: `[FROM, TO]`. If `--to` is omitted, default end is `FROM + weeks - 1 day`. `--to` **requires** `--from`.
- GitHub Actions **CI** workflow: `fmt`, `clippy`, `build`, and `check` on push and PR.
- This **CHANGELOG**.

### Changed
- Documentation refreshed to mention `--to` and CI details.

## [0.2.0] - 2025-10-31
- Minor release with deep rustdoc, README overhaul, and species → Wikipedia **search** links in HTML export.
- Maintains Rust 1.80 compatibility.

## [0.1.37] - 2025-10-31
- Species links switched to Wikipedia **search** for reliability.

## [0.1.36] - 2025-10-31
- HTML export: species names linked to Wikipedia article (superseded by 0.1.37).

## [0.1.35] - 2025-10-31
- `--from YYYY-MM-DD` to scan a fixed **release** window `[FROM, FROM+weeks)`.

## [0.1.34] - 2025-10-31
- Fixes for `Row` visibility and HTML header replacement.

## [0.1.33] - 2025-10-31
- CSV/JSON/HTML exports, 1-decimal `gigabases`, sortable HTML, and ENA links.
