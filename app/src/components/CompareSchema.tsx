import { useState } from "react";
import type { Api } from "../api";
import { errText } from "../hooks";
import type { SchemaChange, SchemaDiff } from "../types";
import type { ObjRef } from "./CompareText";
import type { ConnState } from "./Explorer";

const WORD: Record<string, string> = { only_a: "missing in B", only_b: "extra in B", differs: "differs" };

function ChangeRow({ c }: { c: SchemaChange }) {
  return (
    <tr className={`sc-${c.change}`}>
      <td><span className={`tag ${c.change === "only_b" ? "warn" : ""}`}>{WORD[c.change]}</span></td>
      <td>{c.what}</td><td><b>{c.name}</b></td>
      <td className="small mono">{c.a ?? ""}</td><td className="small mono">{c.b ?? ""}</td>
    </tr>
  );
}

export function CompareSchema({ api, conns, onTextDiff, onWorksheet, onError }: {
  api: Api; conns: ConnState[]; onTextDiff(a: ObjRef, b: ObjRef): void; onWorksheet(connId: string, owner: string, sql: string): void; onError(e: unknown): void;
}) {
  const [a, setA] = useState({ conn: conns[0]?.id ?? "", owner: conns[0]?.schemas[0] ?? "" });
  const [b, setB] = useState({ conn: conns[1]?.id ?? conns[0]?.id ?? "", owner: (conns[1] ?? conns[0])?.schemas[1] ?? (conns[1] ?? conns[0])?.schemas[0] ?? "" });
  const [code, setCode] = useState(true);
  const [busy, setBusy] = useState(false);
  const [res, setRes] = useState<SchemaDiff>();
  const [err, setErr] = useState("");
  const [copied, setCopied] = useState(false);

  const run = async () => {
    setBusy(true); setErr(""); setRes(undefined);
    try { setRes(await api.schemaCompare(a.conn, a.owner, b.conn, b.owner, code)); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  const side = (title: string, v: { conn: string; owner: string }, set: (x: { conn: string; owner: string }) => void) => {
    const c = conns.find((x) => x.id === v.conn);
    return (
      <section className="cmp-src" aria-label={`Schema ${title}`}>
        <header><b>{title}</b><span className="muted small">{title === "A" ? "the reference" : "to be made like A"}</span></header>
        <div className="opt-row">
          <label>Connection<select value={v.conn} onChange={(e) => set({ conn: e.target.value, owner: conns.find((x) => x.id === e.target.value)?.schemas[0] ?? "" })} aria-label={`${title} connection`}>{conns.map((x) => <option key={x.id} value={x.id}>{x.profile.name}</option>)}</select></label>
          <label>Schema<select value={v.owner} onChange={(e) => set({ ...v, owner: e.target.value })} aria-label={`${title} schema`}>{(c?.schemas ?? []).map((s) => <option key={s}>{s}</option>)}</select></label>
        </div>
      </section>
    );
  };
  const copy = async () => { if (!res) return; try { await navigator.clipboard.writeText(res.script); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch (e) { onError(e); } };
  const sm = res?.summary;
  const kindFor = (k: string) => (k === "VIEW" ? "VIEW" : k);

  return (
    <div className="cmp-pane">
      <div className="cmp-srcs">{side("A", a, setA)}{side("B", b, setB)}</div>
      <div className="opt-row cmp-opts">
        <label className="check" title="Reads all PL/SQL source and view text of both schemas and compares it with comments, case and spacing ignored"><input type="checkbox" checked={code} onChange={(e) => setCode(e.target.checked)} /> Compare code (packages, procedures, triggers, views)</label>
        <span className="spacer" />
        <button className="primary" disabled={busy || !a.conn || !b.conn || (a.conn === b.conn && a.owner === b.owner)} onClick={run} title={a.conn === b.conn && a.owner === b.owner ? "Choose two different schemas" : ""}>{busy ? "Reading both schemas…" : "Compare"}</button>
      </div>
      {err && <div className="error" role="alert">{err}</div>}
      {res && sm && (
        <div className="cmp-result schema" aria-label="Schema comparison result">
          <div className={`cmp-stats ${sm.identical ? "same" : ""}`} role="status">
            {sm.identical ? <b>The two schemas match{res.code_compared ? "" : " (code not compared)"}.</b> : <b>{res.owner_b} differs from {res.owner_a}.</b>}
            <span className="muted small"> {sm.tables_only_a} table(s) missing in B, {sm.tables_only_b} extra, {sm.tables_differ} differ ({sm.column_changes} column change(s), {sm.other_changes} other){res.code_compared ? `, code: ${sm.code_differs} differ, ${sm.code_only_a} missing, ${sm.code_only_b} extra` : ""}</span>
          </div>
          {res.tables_only_a.length > 0 && <div><div className="field-label">Tables missing in B</div><div className="chips">{res.tables_only_a.map((t) => <span key={t} className="chipbtn">{t}</span>)}</div></div>}
          {res.tables_only_b.length > 0 && <div><div className="field-label">Tables only in B</div><div className="chips">{res.tables_only_b.map((t) => <span key={t} className="chipbtn">{t}</span>)}</div></div>}
          {res.tables.map((t) => (
            <details key={t.name} open className="sc-table">
              <summary><b>{t.name}</b> <span className="muted small">{t.changes.length} difference{t.changes.length === 1 ? "" : "s"}</span></summary>
              <div className="map-wrap"><table className="map"><thead><tr><th /><th>What</th><th>Name</th><th>In A</th><th>In B</th></tr></thead><tbody>{t.changes.map((c, i) => <ChangeRow key={i} c={c} />)}</tbody></table></div>
            </details>
          ))}
          {res.sequences.length > 0 && <details open className="sc-table"><summary><b>Sequences</b></summary><div className="map-wrap"><table className="map"><tbody>{res.sequences.map((c, i) => <ChangeRow key={i} c={c} />)}</tbody></table></div></details>}
          {res.code.length > 0 && (
            <details open className="sc-table">
              <summary><b>Code</b> <span className="muted small">{res.code.length} object(s)</span></summary>
              <div className="map-wrap"><table className="map" aria-label="Code differences"><tbody>
                {res.code.map((c) => (
                  <tr key={`${c.kind}-${c.name}`}>
                    <td><span className={`tag ${c.change === "only_b" ? "warn" : ""}`}>{WORD[c.change]}</span></td><td>{c.kind.toLowerCase()}</td><td><b>{c.name}</b></td>
                    <td>{c.change === "differs" && <button className="ghost" onClick={() => onTextDiff({ conn: a.conn, owner: a.owner, kind: kindFor(c.kind), name: c.name }, { conn: b.conn, owner: b.owner, kind: kindFor(c.kind), name: c.name })}>Compare text</button>}</td>
                  </tr>
                ))}
              </tbody></table></div>
            </details>
          )}
          <div>
            <div className="opt-row"><div className="field-label grow">Draft script (makes {res.owner_b} look like {res.owner_a}; never run automatically)</div>
              <button onClick={copy}>{copied ? "Copied" : "Copy"}</button>
              <button onClick={() => onWorksheet(b.conn, b.owner, res.script)}>Open in a worksheet</button></div>
            <pre className="kb-preview" aria-label="Draft script" tabIndex={0}>{res.script}</pre>
          </div>
          <ul className="muted small">{res.notes.map((n, i) => <li key={i}>{n}</li>)}</ul>
        </div>
      )}
    </div>
  );
}
