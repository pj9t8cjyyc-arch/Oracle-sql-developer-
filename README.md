# Ora IDE

A fast, lightweight, agent-ready Oracle SQL IDE. Rust core, Tauri shell, React + CodeMirror UI.

## Status: Phase 1 (understand the schema while you work)

| Piece | State |
|---|---|
| `ora-core` safe-mode guard, query sessions, schema browser (Phase 0) | unit-tested |
| `ora-core` lexer + table-reference parser (`parse.rs`) | unit-tested |
| `ora-core` column insight: keys, FKs, checks, indexes (position/leading/unusable), triggers (UPDATE OF / referenced / any update), stats, comments, dependencies | unit-tested on a fake; **dictionary SQL not yet run on a real Oracle** |
| `ora-core` catalog cache + context-aware autocomplete incl. FK-based `JOIN … ON` suggestions | unit-tested (pure function) |
| `ora-core` ranked search (SQLite FTS5): objects, columns, comments, PL/SQL source | unit-tested against real SQLite |
| `ora-core` comment saving (`COMMENT ON …`) with strict quoting | unit-tested incl. hostile input |
| `ora-core` Oracle backend (`--features oracle`) | compiles; **not yet run against a database** |
| `app/` UI: autocomplete, header badges, insight panel, Ctrl+K search, comment editor | builds; behaviour verified in browser **demo mode** |
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
Prints first-page latency and rows/second for a 100k-row stream, and checks the Phase 1 dictionary queries (keys, indexes, triggers, comments, catalog load, completion, search).

## Design notes
- **Lazy paging**: the server-side cursor stays open; the UI asks for 500 rows at a time (virtualised grid) up to a 100k-row cap.
- **Safe mode** (`guard.rs`): lexical analysis that ignores comments/strings. `UPDATE`/`DELETE` without `WHERE`, `DROP`/`TRUNCATE`, and any DML/DDL on a PROD profile require confirmation. Multi-statement input is refused.
- **Credentials**: never written to disk. Profiles store host/service/user only; the password is held in memory for the session. OS keychain support is planned.
- All schema-browser values are bind variables.

- **Insight** (`insight.rs`): one dictionary query per concern, all identifiers bound. A trigger is attached to a column when it lists it (`UPDATE OF`), references it (`:NEW`/`:OLD`, from `ALL_TRIGGER_COLS`), or fires on any UPDATE. Function-based index columns appear under their hidden `SYS_NC…` names (not decoded yet).
- **Result-header badges** only appear for column names that exist in exactly one table referenced by the query; ambiguous names get none rather than wrong ones. Aliased/computed columns get none.
- **Autocomplete** runs on an in-memory catalog snapshot (refreshed per schema; **Reindex** also indexes PL/SQL source, capped at 300k lines). Known limit: a table that follows a parenthesised subquery in the same comma-separated `FROM` list is not seen.
- **Comments** are DDL (immediate, not rollback-able): the exact statement is always shown for confirmation.
- The search index is one SQLite file per `user@host:port/service` in the app data directory.

## Roadmap
1. Phase 0 — connect/browse/run/stream. 2. **Phase 1 — this**: insight, autocomplete, search, comments.
3. Fast export/import + type mapping. 4. Sandbox simulation. 5. Explain plan + optimizer. 6. Own AWR/ASH-style monitor.
