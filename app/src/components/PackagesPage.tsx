import { useEffect, useMemo, useState } from "react";
import type { Api } from "../api";
import { useAsync } from "../hooks";
import { Icon } from "../ui-icons";
import type { PackageDetail, PkgMember } from "../types";

type Filter = "all" | "invalid" | "unreferenced" | "wrapped";

const STATUS: Record<PkgMember["status"], [string, string]> = {
  used: ["used", "ok"],
  unreferenced: ["not referenced", "warn"],
  unused_private: ["unused private", "warn"],
};

export function PackagesPage({ api, conn, owner, select, onOpenSource, onGraph, onOpenTable, onError }: {
  api: Api; conn: string; owner: string; select?: string;
  onOpenSource(pkg: string): void; onGraph(pkg: string): void; onOpenTable(table: string): void; onError(e: unknown): void;
}) {
  const res = useAsync(() => api.packageAnalysis(conn, owner, false), [api, conn, owner]);
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [sel, setSel] = useState<string>();
  const [onlyFlagged, setOnlyFlagged] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const a = res.data;

  useEffect(() => { if (select) setSel(select.toUpperCase()); }, [select]);
  useEffect(() => { if (a && !sel && a.packages.length) setSel(a.packages[0].name); }, [a, sel]);

  const list = useMemo(() => {
    if (!a) return [];
    const t = q.trim().toUpperCase();
    return a.packages.filter((p) => {
      if (t && !p.name.includes(t) && !p.members.some((m) => m.name.includes(t))) return false;
      if (filter === "invalid") return p.spec_status !== "VALID" || (p.body_status ?? "VALID") !== "VALID";
      if (filter === "unreferenced") return p.unreferenced > 0;
      if (filter === "wrapped") return p.wrapped;
      return true;
    });
  }, [a, q, filter]);
  const cur: PackageDetail | undefined = a?.packages.find((p) => p.name === sel);

  const refresh = async () => {
    setRefreshing(true);
    try { await api.packageAnalysis(conn, owner, true); res.reload(); } catch (e) { onError(e); } finally { setRefreshing(false); }
  };

  const counts = a ? { invalid: a.invalid, wrapped: a.wrapped, unreferenced: a.packages.filter((p) => p.unreferenced > 0).length } : undefined;

  return (
    <div className="pk-page">
      <aside className="pk-list">
        <div className="pk-tools">
          <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter packages or members…" aria-label="Filter packages" />
          <button className="ghost icon" title="Re-read all package bodies" disabled={refreshing} onClick={refresh} aria-label="Reload packages"><Icon name="refresh" size={14} /></button>
        </div>
        <div className="pk-filters" role="radiogroup" aria-label="Package filter">
          {([["all", "All"], ["invalid", "Invalid"], ["unreferenced", "Unreferenced"], ["wrapped", "Wrapped"]] as [Filter, string][]).map(([k, l]) => (
            <button key={k} role="radio" aria-checked={filter === k} className={`chipbtn ${filter === k ? "on" : ""}`} onClick={() => setFilter(k)}>
              {l}{counts && k !== "all" && <span className="count">{counts[k]}</span>}
            </button>
          ))}
        </div>
        <div className="pk-rows" role="listbox" aria-label="Packages">
          {res.loading && <div className="muted pad">Reading package bodies…</div>}
          {res.error && <div className="error pad">{res.error}</div>}
          {a && list.length === 0 && <div className="muted pad">{a.packages.length === 0 ? `No packages in ${owner}.` : "No package matches."}</div>}
          {list.map((p) => {
            const bad = p.spec_status !== "VALID" || (p.body_status ?? "VALID") !== "VALID";
            return (
              <button key={p.name} role="option" aria-selected={p.name === sel} className={`pk-row ${p.name === sel ? "on" : ""}`} onClick={() => setSel(p.name)}>
                <span className={`dot ${bad ? "bad" : "ok"}`} title={bad ? "Invalid" : "Valid"} />
                <span className="pk-name">{p.name}</span>
                {p.wrapped && <span className="tag">wrapped</span>}
                {p.analysis === "plscope" && <span className="tag ok" title="Analysed with PL/Scope: exact, with line numbers">exact</span>}
                {p.unreferenced > 0 && <span className="tag warn" title="Public members nothing in this schema calls">{p.unreferenced} unref.</span>}
                <span className="count">{p.members.length}</span>
              </button>
            );
          })}
        </div>
        {a && <div className="pk-foot muted small">{a.packages.length} packages · {a.packages.reduce((n, p) => n + p.members.length, 0)} members
          <br />{a.plscope_packages} analysed with PL/Scope (exact), {a.packages.filter((p) => p.analysis === "scan").length} by text scan{a.scope_error && <span className="warn" title={a.scope_error}> · PL/Scope unavailable</span>}</div>}
      </aside>

      <section className="pk-detail">
        {!cur && !res.loading && <div className="muted pad">Select a package.</div>}
        {cur && (
          <>
            <header className="pk-head">
              <h2>{owner}.<b>{cur.name}</b></h2>
              <span className={`chip ${cur.spec_status === "VALID" ? "ok" : "bad"}`}>spec {cur.spec_status.toLowerCase()}</span>
              {cur.body_status && <span className={`chip ${cur.body_status === "VALID" ? "ok" : "bad"}`}>body {cur.body_status.toLowerCase()}</span>}
              {cur.wrapped && <span className="chip">wrapped: not scanned</span>}
              {!cur.wrapped && <span className={`chip ${cur.analysis === "plscope" ? "ok" : ""}`} title={cur.analysis === "plscope" ? "Calls and tables come from PL/Scope" : "Calls and tables come from a text scan"}>{cur.analysis === "plscope" ? "exact · PL/Scope" : "text scan"}</span>}
              <span className="spacer" />
              <button onClick={() => onOpenSource(cur.name)}><Icon name="sql" size={14} /> Source</button>
              <button onClick={() => onGraph(cur.name)}><Icon name="out" size={14} /> Graph</button>
            </header>
            {cur.analysis === "plscope" ? (
              <div className="ok-box small" role="note">
                Calls and table usage come from <b>PL/Scope</b>: exact, with line numbers. Dynamic SQL and callers outside this schema (jobs, applications) are still not seen.
                {cur.unattributed > 0 && <> {cur.unattributed} usage(s) could not be attributed to a member.</>}
              </div>
            ) : (
              <div className="warn-box small" role="note">
                Calls and table usage come from a text scan of the package body (<b>heuristic</b>): dynamic SQL, callers outside this schema and the second and later tables of a
                <code> FROM a, b</code> list are not seen. "Not referenced" means <i>no caller found here</i>, not "safe to delete".
                {!cur.wrapped && <> For exact data compile it with PL/Scope: <code className="block">ALTER SESSION SET plscope_settings = 'IDENTIFIERS:ALL, STATEMENTS:ALL'; ALTER PACKAGE {cur.name.toLowerCase()} COMPILE BODY;</code></>}
              </div>
            )}

            <div className="pk-links">
              <div><span className="field-label">Reads / writes</span>
                <div className="chips">
                  {cur.tables.length === 0 && <span className="muted small">{cur.wrapped ? "unknown (wrapped)" : "no tables found"}</span>}
                  {cur.tables.map((t) => (
                    <button key={t.table} className="chipbtn" onClick={() => onOpenTable(t.table)} title={`${t.read ? "reads" : ""}${t.read && t.write ? " and " : ""}${t.write ? "writes" : ""} ${t.table}`}>
                      {t.table} <span className={`rw ${t.write ? "w" : "r"}`}>{t.read && t.write ? "R/W" : t.write ? "W" : "R"}</span>
                    </button>
                  ))}
                </div></div>
              <div><span className="field-label">Calls</span>
                <div className="chips">{cur.calls_packages.length === 0 ? <span className="muted small">none</span> : cur.calls_packages.map((p) => <button key={p} className="chipbtn" onClick={() => setSel(p)}>{p}</button>)}</div></div>
              <div><span className="field-label">Called by</span>
                <div className="chips">{cur.called_by.length === 0 ? <span className="muted small">no package in this schema</span> : cur.called_by.map((p) => <button key={p} className="chipbtn" onClick={() => setSel(p)}>{p}</button>)}</div></div>
            </div>

            <div className="pk-members-head">
              <span className="field-label">{cur.members.length} members
                {cur.unreferenced + cur.unused_private > 0 && <span className="muted small"> · {cur.unreferenced} unreferenced, {cur.unused_private} unused private</span>}</span>
              <label className="check small"><input type="checkbox" checked={onlyFlagged} onChange={(e) => setOnlyFlagged(e.target.checked)} /> only unreferenced</label>
            </div>
            <div className="pk-table-wrap">
              <table className="map pk-members" aria-label="Package members">
                <thead><tr><th>Member</th><th>Scope</th><th>Reads</th><th>Writes</th><th>Calls</th><th>Called by</th><th>Status</th></tr></thead>
                <tbody>
                  {cur.members.filter((m) => !onlyFlagged || m.status !== "used").map((m, i) => (
                    <tr key={`${m.name}-${m.overload ?? ""}-${i}`}>
                      <td><span className="kw">{m.kind === "FUNCTION" ? "fn" : "proc"}</span> <span className="mono">{m.signature}</span></td>
                      <td className="small">{m.public ? "public" : "private"}</td>
                      <td className="small">{m.reads.join(", ") || "–"}</td>
                      <td className="small">{m.writes.join(", ") || "–"}</td>
                      <td className="small" title={m.scan_only.length ? `The text scan also saw (PL/Scope did not): ${m.scan_only.join(", ")}` : m.calls.map((c) => `${c.package}.${c.member}`).join("\n")}>{m.calls.length ? m.calls.map((c) => `${c.package}.${c.member}`).join(", ") : m.internal_refs ? `${m.internal_refs} internal` : "–"}</td>
                      <td className="small">{m.called_by.length ? m.called_by.map((c) => `${c.package}.${c.member}`).join(", ") : m.internal_refs ? `${m.internal_refs} internal` : "–"}</td>
                      <td className={`small st-${STATUS[m.status][1]}`}>{cur.wrapped ? "not scanned" : STATUS[m.status][0]}{m.scan_only.length > 0 && <div className="muted">scan also saw: {m.scan_only.join(", ")}</div>}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
      </section>
    </div>
  );
}
