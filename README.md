# Ora IDE

A fast, lightweight, agent-ready Oracle SQL IDE. Rust core, Tauri shell, React + CodeMirror UI.

## Status: history & worksheets (local SQLite)

| Piece | State |
|---|---|
| Every executed statement recorded (when, how long, rows, ok/error/cancelled, which connection/schema/worksheet) | 102 core tests; recording hooked into the Tauri `run_query` command |
| Search over all history (words, tables, columns), ranked by relevance + run count + recency; starred queries; filters by connection, time, kind, success | tested on real SQLite FTS5; **21 ms over 20,000 executions** |
| Worksheets autosaved (≈0.7 s after typing), restored on next start, archived on close, searchable | UI verified in a browser incl. a real page reload (demo backend) |
| Statement outline + auto-title per worksheet; step-by-step trail of what was actually run | tested |
| Ctrl+Enter runs the statement under the cursor; F5 / *Run script* runs all in order, stopping at the first failure | UI verified |
| "You ran this before" suggestions in the editor and in Ctrl+K | UI verified |
| Everything above **against a real Oracle** | **not run** (no database here) |

**Where it lives:** one file, `workspace.sqlite`, in the app data directory (WAL mode). Query text is stored once per distinct query (case/whitespace/comment-insensitive, literals kept) and every run is a small row pointing at it. Search is FTS5; nothing is sent anywhere.

**Privacy:** passwords are masked before anything is written to history (`IDENTIFIED BY`, `IDENTIFIED BY VALUES`, `REPLACE`, SQL*Plus `CONNECT user/pass`), including inside stored error messages. Worksheet text is stored exactly as typed. The file is **not encrypted**: use OS disk encryption on shared machines. *Recording* can be paused from the History page.

**Not built yet:** automatic retention (`history_purge` exists as a command but has no UI), export/backup of the history file, per-environment "never record" rules.

## Plan: what comes next (to discuss)

1. **Package explorer** (you have 300–400 packages): index every package member from `ALL_PROCEDURES` / `ALL_ARGUMENTS` (signatures, overloads), keep the source in the existing FTS5 index, and show per package: members with signatures, who calls it, which tables it reads or writes, size, invalid/wrapped state, and members nothing references (dead-code candidates). Calls come from PL/Scope (`ALL_IDENTIFIERS`) when the code was compiled with `PLSCOPE_SETTINGS='IDENTIFIERS:ALL'`, otherwise from our own parser (heuristic, labelled as such). A virtualised tree and lazy source loading keep 400 packages instant.
2. **Linkage graph tab**: nodes = tables, views, packages, procedures, triggers, sequences; edges = foreign keys, `ALL_DEPENDENCIES`, trigger→table/sequence. Two questions answered directly: *"what breaks if I change this?"* (used-by closure) and *"what does this need?"* (uses closure), with hop limits and type filters so a big schema stays readable. The graph data is cached in the same SQLite file and traversed with recursive queries.
3. **AI** (needs your input): where your existing AI code runs (Rust, Python sidecar, remote service) and which provider decides the shape. My suggestion: a provider trait with a local-model option; send **metadata and your own history, never row data**, by default; uses in this order: (a) NL→SQL grounded in the schema *and the queries you have already run*, (b) explain a package/trigger, (c) worksheet titles and summaries, (d) optimisation hints next to the explain plan. Anything the AI writes passes the same safe-mode guard, and only read-only statements run without confirmation.
4. **DuckDB**: keep for analytics, not as a second copy of history. It can `ATTACH` the SQLite file directly, so "how did this query's runtime change over a year" needs no duplicate. DuckDB's main job stays the AWR/ASH-style monitor and fast file profiling for import/export.
5. Still pending from before: fast parallel export/import with type mapping, sandbox simulation, explain-plan viewer, and running the ignored integration tests against a real Oracle.

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
- History records the time to first rows (or to completion for non-queries), not the time to scroll through a long result; the row count is updated as you scroll.
- Worksheets on the same connection share one session (and transaction); Commit/Rollback apply to the connection, not the tab.
- Explorer lists are capped at 300 rows per kind (use the filter); there is no virtualised tree yet.
- The search index is one SQLite file per `user@host:port/service` in the app data directory.

## Roadmap
1. Phase 0 — connect/browse/run/stream. 2. **Phase 1 — this**: insight, autocomplete, search, comments.
3. Fast export/import + type mapping. 4. Sandbox simulation. 5. Explain plan + optimizer. 6. Own AWR/ASH-style monitor.
