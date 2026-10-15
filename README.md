# Ora IDE

A fast, lightweight, agent-ready Oracle SQL IDE. Rust core, Tauri shell, React + CodeMirror UI.

## Status: Phase 0 (connect, browse, run, stream)

| Piece | State |
|---|---|
| `crates/ora-core` safe-mode guard, query sessions, schema browser | unit-tested (21 tests) |
| `crates/ora-core` Oracle backend (`--features oracle`) | compiles; **not yet run against a database** — see below |
| `app/` React UI | builds; behaviour verified in browser **demo mode** (mock backend) |
| `app/src-tauri` desktop shell | compiles; not yet launched on a desktop |

### Try it

```bash
cargo test -p ora-core                      # core tests, no database needed
cd app && npm ci && npm run dev             # UI in demo mode (mock data) at :1420
cd app && npm run tauri dev                 # real app; needs Oracle Instant Client + a database
```

### Verify the Oracle backend (do this first)

```bash
ORA_HOST=localhost ORA_SERVICE=FREEPDB1 ORA_USER=system ORA_PASSWORD=... \
cargo test -p ora-core --features oracle --test oracle_it -- --ignored --nocapture
```
Prints first-page latency and total rows/second for a 100k-row stream.

## Design notes
- **Lazy paging**: the server-side cursor stays open; the UI asks for 500 rows at a time (virtualised grid) up to a 100k-row cap.
- **Safe mode** (`guard.rs`): lexical analysis that ignores comments/strings. `UPDATE`/`DELETE` without `WHERE`, `DROP`/`TRUNCATE`, and any DML/DDL on a PROD profile require confirmation. Multi-statement input is refused.
- **Credentials**: never written to disk. Profiles store host/service/user only; the password is held in memory for the session. OS keychain support is planned.
- All schema-browser values are bind variables.

## Roadmap
1. Phase 0 — this. 2. Column insight panel + search. 3. Fast export/import + type mapping.
4. Sandbox simulation. 5. Explain plan + optimizer. 6. Own AWR/ASH-style monitor.
