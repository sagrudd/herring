# herring 🐟

**v0.1.35** — Adds `--from YYYY-MM-DD` to scan a closed **release** window: `[FROM, FROM + weeks)`.  
Example: `--from 2024-01-01 --weeks 12` lists studies with `first_public` in that 12‑week span.

- Default behavior (no `--from`) remains: runs where `first_public >= now-weeks` **or** `last_updated >= now-weeks`.
- With `--from`, we query **released-only** datasets using `first_public` in the window.

Examples:
```bash
# Last 8 weeks (rolling)
herring list -vv

# Fixed window: 12 weeks from 2024-01-01 (released only)
herring list -vv --from 2024-01-01 --weeks 12
```
