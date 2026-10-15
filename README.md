# Ora IDE

A fast, lightweight, agent-ready Oracle SQL IDE. Rust core, Tauri shell, React + CodeMirror UI.

## Status: PL/Scope (exact package usage)

| Piece | State |
|---|---|
| Packages compiled with `PLSCOPE_SETTINGS='IDENTIFIERS:ALL[, STATEMENTS:ALL]'` are analysed from `ALL_IDENTIFIERS` / `ALL_STATEMENTS`: exact callee (resolved through the identifier signature), every table in a `FROM a, b` list, synonyms resolved, `EXECUTE IMMEDIATE` sites, all with line numbers | 8 new core tests with a fake database (fusion rules, fallback, loader) |
| Other packages keep the text scan; each package, member and link says which one it came from (`plscope / exact` vs `scan / heuristic`) | UI verified in demo mode |
| Packages tab shows "exact · PL/Scope" or "text scan" per package, how many of each, and the exact `ALTER SESSION … COMPILE BODY` command to upgrade a scan-only package | UI verified |
| Anything PL/Scope did not report but the scan saw is kept as "scan also saw" for review, not trusted | tested |
| PL/Scope usages that fall outside every member's lines are counted and reported, never dropped silently | tested |
| Everything above **against a real Oracle** | **not run**. The three PL/Scope queries are written from the documented view columns and were never executed against a database: if one fails, the app falls back to the text scan and shows the error. Ignored test `plscope_gives_exact_calls_and_tables_for_packages_compiled_with_it` checks them on a live schema |

**How the pieces are combined:** PL/Scope decides *what* is called or touched; a table's read/write is taken from the statement type PL/Scope recorded, except that an `INSERT … SELECT` source table stays a read (PL/Scope reports the whole statement as `INSERT`). Usages are attributed to members by line range. Calls to standalone procedures/functions are not resolved here (they come from dictionary dependencies).

## Status: knowledge base & markdown export (Phase 4)

| Piece | State |
|---|---|
| **Knowledge base** per schema ("Build knowledge base"): tables, views, columns (with comments), keys, indexes, triggers, sequences, synonyms, grants, scheduler jobs, packages (members, lines, reads/writes/calls) and every dependency, including objects in *other* schemas that use yours | 178 core tests in total (5 for the knowledge base, using a fake database) |
| **Every link records its source and certainty**: `dictionary / exact` (Oracle's data dictionary: dependencies, foreign keys with column pairs, indexes, triggers, synonyms, jobs) or `scan / heuristic` (package-body text scan, with member and line numbers). Where both exist the more specific kind wins (`writes` > `reads` > `calls` > `fk` > `indexes` > `uses`) and the evidence keeps every reason | tested |
| **Coverage report**: what the knowledge base *cannot* see is listed, not hidden: wrapped packages, dynamic-SQL sites (with member), invalid objects, database-link synonyms, unreadable grants/jobs, external callers | tested |
| Stored in the local SQLite file (latest snapshot per database and schema); survives restarts | tested |
| **Markdown export** from any table/package page or the graph tab, one click: a single `.md`, or a `.zip` with one file per object, `README.md` index, `graph.json` (nodes, edges, evidence) and `manifest.json`. Each file has front matter with its id and typed links so a model or tool can follow them | 7 dossier tests; UI verified in demo mode |
| A **table** file contains: columns (type, null, keys, comments), constraints and foreign keys both ways, indexes, **triggers with source**, sequences, **views with SQL**, **packages that use it with the member code and line numbers**, other schemas that use it, grants, DDL (from `DBMS_METADATA`) | tested |
| A **package** file contains: spec, every member with signature, reads/writes/calls/called-by, dynamic-SQL lines, "no caller found", and the member code | tested |
| Everything above **against a real Oracle** | **not run**: ignored test `knowledge_base_and_markdown_dossier_cover_a_known_schema` creates tables, a trigger, a sequence and a package and checks the result |

**"Don't miss anything", honestly:** no tool can promise that. What is guaranteed here is that a miss is *visible*: the README of every export lists what could not be seen, and text that could not be read is listed as such.

**Privacy:** row data is never included. Source text is masked for `IDENTIFIED BY …` and for credential-looking literals (`password := '…'`, `api_key => '…'`), **including inside comments**. This is best-effort pattern masking, not a guarantee: read an export before you share it. Exports are written only where you choose.

**Not built yet:** PL/Scope as an exact source for calls, column-level lineage inside code, incremental refresh (a rebuild re-reads everything), semantic/implicit relationships (columns that join without a foreign key), AI-proposed links.

## Status: package explorer & linkage graph (Phase 3)

| Piece | State |
|---|---|
| **Packages tab** (top bar, or "Members & usage" on a package): every package in a schema with exact signatures (`ALL_PROCEDURES`/`ALL_ARGUMENTS`), public vs private members, tables read/written, packages called, callers, and members nothing references | 14 new core tests; **400 packages / 16,000 members analysed in 156 ms** (release, excluding the database) |
| Filters: invalid, with unreferenced members, wrapped; filter by package or member name | UI verified in demo mode |
| **Graph tab** (top bar, or "Graph" on any object): pick an object, see *what it uses*, *what uses it* (what breaks if you change it) or both, 1–6 hops, filter by kind; click re-centres, double-click opens the object | tested (BFS, cycles, caps, kind filter) + UI verified |
| Everything above **against a real Oracle** | **not run**: ignored test `package_analysis_and_dependency_graph_match_a_known_schema` creates two packages and two tables and checks the result |

**How it works:** the schema's dictionary data and package bodies are loaded once and cached (Reload button re-reads); analysis and graph traversal then run in memory, so changing focus, direction or hops never touches the database.

**Heuristic, said plainly:** calls and table usage come from a lexical scan of the package bodies, not from PL/Scope. It does not see dynamic SQL, callers outside the schema (jobs, applications), or the second and later tables of a `FROM a, b` list. "Not referenced" means *no caller found in this schema*, not "safe to delete". Wrapped bodies are listed but cannot be scanned. Graph edges come from `ALL_DEPENDENCIES` (the object's own schema) and foreign keys; objects in other schemas show up when this schema uses them.

**Not built yet:** PL/Scope as an exact source for calls, column-level lineage, persisting the analysis in the local SQLite file, exporting the graph as an image.

## Status: import & export (Phase 2)

| Piece | State |
|---|---|
| Export a table or any query to **CSV, TSV, Excel (.xlsx), JSON Lines, SQL INSERT** — streamed from the cursor, optional gzip, split into several files, atomic write (`.part` renamed when finished, removed on cancel/error) | 149 core tests |
| **"Best option" recommendation** by row count and purpose (Excel / load into Oracle / share with a tool / archive), with a verdict per format and the reason | tested; UI verified in demo mode |
| Import **CSV, TSV, text, .gz, .xlsx**: encoding, delimiter and header detection; per-column type inference; ambiguous dates (`03/04/2021`) and decimal marks (`1.234,56`) are flagged and you choose | tested; UI verified |
| Column mapping against the target table (by name, else position), with a fix suggestion (e.g. `ALTER TABLE … MODIFY …`) for every problem; *Create table from file* shows the DDL first | tested |
| *Check file* validates every row without touching the database; the load maps Oracle row errors back to **file row numbers** and writes a reject file | tested with a fake database |
| Everything above **against a real Oracle** | **not run** — ignored integration tests exist (`oracle_it.rs`) |

**Which format for large data?** CSV (add gzip) is the default for anything big: constant memory, any tool reads it, ~5× smaller compressed. Excel stops at 1,048,576 rows per sheet, so large Excel exports are split into files that each open; above ~100k rows Excel is marked "caution". SQL INSERT is for small sets you want to review or replay (above ~200k rows it is marked "avoid"). For tens of millions of rows between two Oracle databases the app points you to Data Pump (`expdp`/`impdp`), which runs inside the database and beats any client-side tool.

**Measured (writer side, release build, no database):** CSV ≈ 990k rows/s, CSV.gz ≈ 1.09M rows/s, JSONL ≈ 510k, SQL INSERT ≈ 620k, XLSX ≈ 145k; import convert + batch ≈ 570k rows/s. Real throughput is bound by the network and the database; run the ignored `export_throughput`/`import_throughput` tests and `oracle_it` for your numbers.

**Safety:** dates and numbers are written in fixed formats whatever the session NLS is; text cells starting with `= + - @` are neutralised for Excel (non-numeric columns only); JSON/XLSX keep numbers above 15 significant digits as text so nothing is silently rounded. Imports use explicit `TO_NUMBER/TO_DATE/TO_TIMESTAMP` formats, bind arrays, commit once at the end by default, and ask for an extra confirmation on PROD. Leading zeros stay text unless the target column is numeric.

**Limits:** Oracle 12.1+ is needed to map batch errors to rows; export/import run on a second session opened with the password held in memory; Excel files are read fully into memory; `TIMESTAMP WITH TIME ZONE`, `BLOB`/`CLOB` and similar columns are not imported; parallel (multi-session) export is not implemented.

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

1. ~~**Package explorer**~~ **built (Phase 3)**; original plan: (you have 300–400 packages): index every package member from `ALL_PROCEDURES` / `ALL_ARGUMENTS` (signatures, overloads), keep the source in the existing FTS5 index, and show per package: members with signatures, who calls it, which tables it reads or writes, size, invalid/wrapped state, and members nothing references (dead-code candidates). Calls come from PL/Scope (`ALL_IDENTIFIERS`) when the code was compiled with `PLSCOPE_SETTINGS='IDENTIFIERS:ALL'`, otherwise from our own parser (heuristic, labelled as such). A virtualised tree and lazy source loading keep 400 packages instant.
2. ~~**Linkage graph tab**~~ **built (Phase 3)**; original plan: nodes = tables, views, packages, procedures, triggers, sequences; edges = foreign keys, `ALL_DEPENDENCIES`, trigger→table/sequence. Two questions answered directly: *"what breaks if I change this?"* (used-by closure) and *"what does this need?"* (uses closure), with hop limits and type filters so a big schema stays readable. The graph data is cached in the same SQLite file and traversed with recursive queries.
3. **AI** (needs your input): where your existing AI code runs (Rust, Python sidecar, remote service) and which provider decides the shape. My suggestion: a provider trait with a local-model option; send **metadata and your own history, never row data**, by default; uses in this order: (a) NL→SQL grounded in the schema *and the queries you have already run*, (b) explain a package/trigger, (c) worksheet titles and summaries, (d) optimisation hints next to the explain plan. Anything the AI writes passes the same safe-mode guard, and only read-only statements run without confirmation.
4. **DuckDB**: keep for analytics, not as a second copy of history. It can `ATTACH` the SQLite file directly, so "how did this query's runtime change over a year" needs no duplicate. DuckDB's main job stays the AWR/ASH-style monitor and fast file profiling for import/export.
5. Still pending from before: parallel export, sandbox simulation, explain-plan viewer, and running the ignored integration tests against a real Oracle.

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
1. Phase 0 — connect/browse/run/stream. 2. Phase 1: insight, autocomplete, search, comments. 3. **Fast export/import + type mapping — done (Phase 2)**. 4. Sandbox simulation. 5. Explain plan + optimizer. 6. Own AWR/ASH-style monitor.
