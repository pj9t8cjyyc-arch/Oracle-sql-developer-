# Ora IDE

A fast, lightweight, agent-ready Oracle SQL IDE. Rust core, Tauri shell, React + CodeMirror UI.

## Status: workspace redesign (multi-connection dictionary browser)

| Piece | State |
|---|---|
| Multiple simultaneous connections (Tauri state keyed by connection id; every command takes `conn`) | compiles; **not run against real databases** |
| Explorer: connections → schemas → kinds (tables, views, mviews, indexes, sequences, triggers, procedures, functions, packages, types, synonyms) with counts; invalid/unusable/disabled flagged | UI verified in demo mode |
| Object pages: table overview (indexes, triggers, sequences, constraints, referenced-by at a glance), column focus pane, per-kind pages with source/DDL/dependencies | UI verified in demo mode |
| Sequences: identity columns, trigger-assigned (`:NEW.col := seq.NEXTVAL`, parsed from trigger source), table dependencies | 79 unit tests; **dictionary SQL not run on a real Oracle** |
| Cross-connection search (Ctrl/Cmd+K) | UI verified in demo mode |
| Phase 0/1 (guard, streaming, insight, autocomplete, FTS5 search, comments) | as before |

**Sequence attribution** — Oracle has no table→sequence link, so each use is tagged with how it was found: `identity` (exact), `trigger` (exact column, from parsing the trigger body; dynamic SQL or indirection through a variable is not detected) or `default` (the table depends on the sequence, e.g. a column DEFAULT; the column is *not* guessed).

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
Prints first-page latency and rows/second for a 100k-row stream, and checks the dictionary queries (keys, indexes, triggers, sequences, comments, catalog load, completion, search, object listing, source, DDL, dependencies). Sequence tests need Oracle 12c+ (identity / DEFAULT seq.NEXTVAL).

## Design notes
- **Lazy paging**: the server-side cursor stays open; the UI asks for 500 rows at a time (virtualised grid) up to a 100k-row cap.
- **Safe mode** (`guard.rs`): lexical analysis that ignores comments/strings. `UPDATE`/`DELETE` without `WHERE`, `DROP`/`TRUNCATE`, and any DML/DDL on a PROD profile require confirmation. Multi-statement input is refused.
- **Credentials**: never written to disk. Profiles store host/service/user only; the password is held in memory for the session. OS keychain support is planned.
- All schema-browser values are bind variables.

- **Insight** (`insight.rs`): one dictionary query per concern, all identifiers bound. A trigger is attached to a column when it lists it (`UPDATE OF`), references it (`:NEW`/`:OLD`, from `ALL_TRIGGER_COLS`), or fires on any UPDATE. Function-based index columns appear under their hidden `SYS_NC…` names (not decoded yet).
- **Result-header badges** only appear for column names that exist in exactly one table referenced by the query; ambiguous names get none rather than wrong ones. Aliased/computed columns get none.
- **Autocomplete** runs on an in-memory catalog snapshot (refreshed per schema; **Reindex** also indexes PL/SQL source, capped at 300k lines). Known limit: a table that follows a parenthesised subquery in the same comma-separated `FROM` list is not seen.
- **Comments** are DDL (immediate, not rollback-able): the exact statement is always shown for confirmation.
- Worksheets on the same connection share one session (and transaction); Commit/Rollback apply to the connection, not the tab.
- Explorer lists are capped at 300 rows per kind (use the filter); there is no virtualised tree yet.
- The search index is one SQLite file per `user@host:port/service` in the app data directory.

## Roadmap
1. Phase 0 — connect/browse/run/stream. 2. **Phase 1 — this**: insight, autocomplete, search, comments.
3. Fast export/import + type mapping. 4. Sandbox simulation. 5. Explain plan + optimizer. 6. Own AWR/ASH-style monitor.
