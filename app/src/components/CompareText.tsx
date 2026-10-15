import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../api";
import type { TextDiffResult, TextOptions } from "../types";
import { DiffView } from "./DiffView";
import type { ConnState } from "./Explorer";

export interface ObjRef { conn: string; owner: string; kind: string; name: string }
export interface SheetRef { id: string; title: string; sql: string }
interface Src { label: string; text: string }
type Mode = "file" | "paste" | "sheet" | "object";

const KINDS: [string, boolean][] = [["PACKAGE", false], ["PACKAGE BODY", false], ["PROCEDURE", false], ["FUNCTION", false], ["TRIGGER", false], ["TYPE", false], ["TYPE BODY", false], ["VIEW", true], ["TABLE", true], ["INDEX", true], ["SEQUENCE", true]];
const OPTS: [keyof TextOptions, string][] = [["ignore_whitespace", "Ignore spaces"], ["ignore_case", "Ignore case"], ["ignore_sql_comments", "Ignore SQL comments"], ["ignore_blank_lines", "Ignore blank lines"]];

function SourcePicker({ api, conns, sheets, title, value, onChange, onError, preset }: {
  api: Api; conns: ConnState[]; sheets: SheetRef[]; title: string; value: Src; onChange(s: Src): void; onError(e: unknown): void; preset?: ObjRef;
}) {
  const [mode, setMode] = useState<Mode>("paste");
  const [obj, setObj] = useState<ObjRef>({ conn: conns[0]?.id ?? "", owner: conns[0]?.schemas[0] ?? "", kind: "PACKAGE BODY", name: "" });
  const [busy, setBusy] = useState(false);
  const conn = conns.find((c) => c.id === obj.conn);

  const fetchObj = useCallback(async (o: ObjRef) => {
    setBusy(true);
    try {
      const ddl = KINDS.find((k) => k[0] === o.kind)?.[1];
      const text = await (ddl ? api.objectDdl(o.conn, o.owner, o.name, o.kind) : api.objectSource(o.conn, o.owner, o.name, o.kind));
      if (!text.trim()) throw new Error(`${o.kind.toLowerCase()} ${o.owner}.${o.name} has no readable text (wrapped or not visible)`);
      const c = conns.find((x) => x.id === o.conn);
      onChange({ label: `${c?.profile.name ?? ""} · ${o.owner}.${o.name} (${o.kind.toLowerCase()})`, text });
    } catch (e) { onError(e); } finally { setBusy(false); }
  }, [api, conns, onChange, onError]);

  const lastPreset = useRef<ObjRef>();
  useEffect(() => {
    if (preset && preset !== lastPreset.current) { lastPreset.current = preset; setMode("object"); setObj(preset); void fetchObj(preset); }
  }, [preset, fetchObj]);

  const browse = async () => {
    try {
      const p = await api.pickOpenPath([]);
      if (!p) return;
      onChange({ label: p, text: await api.textReadFile(p) });
    } catch (e) { onError(e); }
  };

  return (
    <section className="cmp-src" aria-label={title}>
      <header><b>{title}</b>
        <div className="segmented tight" role="radiogroup" aria-label={`${title} source`}>
          {([["file", "File"], ["paste", "Paste"], ["sheet", "Worksheet"], ["object", "Database object"]] as [Mode, string][]).map(([m, l]) => (
            <button key={m} role="radio" aria-checked={mode === m} className={mode === m ? "on" : ""} onClick={() => setMode(m)}>{l}</button>
          ))}
        </div>
      </header>
      {mode === "file" && <div className="opt-row grow"><button onClick={browse}>Browse…</button><span className="muted small mono grow">{value.label || "no file chosen"}</span></div>}
      {mode === "paste" && <textarea aria-label={`${title} text`} rows={6} spellCheck={false} placeholder="Paste text here" value={value.text} onChange={(e) => onChange({ label: "pasted text", text: e.target.value })} />}
      {mode === "sheet" && (
        <select aria-label={`${title} worksheet`} value="" onChange={(e) => { const s = sheets.find((x) => x.id === e.target.value); if (s) onChange({ label: `worksheet ${s.title}`, text: s.sql }); }}>
          <option value="" disabled>choose an open worksheet…</option>
          {sheets.map((s) => <option key={s.id} value={s.id}>{s.title}</option>)}
        </select>
      )}
      {mode === "object" && (
        <div className="opt-row">
          <label>Connection<select value={obj.conn} onChange={(e) => { const c = conns.find((x) => x.id === e.target.value); setObj({ ...obj, conn: e.target.value, owner: c?.schemas[0] ?? "" }); }}>{conns.map((c) => <option key={c.id} value={c.id}>{c.profile.name}</option>)}</select></label>
          <label>Schema<select value={obj.owner} onChange={(e) => setObj({ ...obj, owner: e.target.value })}>{(conn?.schemas ?? []).map((s) => <option key={s}>{s}</option>)}</select></label>
          <label>Kind<select value={obj.kind} onChange={(e) => setObj({ ...obj, kind: e.target.value })}>{KINDS.map(([k]) => <option key={k}>{k}</option>)}</select></label>
          <label className="grow">Name<input value={obj.name} onChange={(e) => setObj({ ...obj, name: e.target.value.toUpperCase() })} placeholder="EMP_PKG" aria-label={`${title} object name`} /></label>
          <button disabled={!obj.name.trim() || busy} onClick={() => fetchObj(obj)}>{busy ? "Loading…" : "Load"}</button>
        </div>
      )}
      <div className="muted small" aria-label={`${title} loaded`}>{value.text ? `${value.label} · ${value.text.split("\n").length.toLocaleString()} lines` : "nothing loaded yet"}</div>
    </section>
  );
}

export function CompareText({ api, conns, sheets, preset, onError }: {
  api: Api; conns: ConnState[]; sheets: SheetRef[]; preset?: { a: ObjRef; b: ObjRef; nonce: number }; onError(e: unknown): void;
}) {
  const [a, setA] = useState<Src>({ label: "", text: "" });
  const [b, setB] = useState<Src>({ label: "", text: "" });
  const [opts, setOpts] = useState<TextOptions>({ ignore_whitespace: false, ignore_case: false, ignore_sql_comments: false, ignore_blank_lines: false });
  const [view, setView] = useState<"side" | "unified">("side");
  const [context, setContext] = useState<number | null>(3);
  const [res, setRes] = useState<TextDiffResult>();
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState("");
  const [copied, setCopied] = useState(false);
  const seq = useRef(0);

  const run = useCallback(async () => {
    const id = ++seq.current;
    setBusy(true);
    try { const r = await api.textDiff(a.text, b.text, opts, context); if (id === seq.current) setRes(r); }
    catch (e) { onError(e); } finally { if (id === seq.current) setBusy(false); }
  }, [api, a.text, b.text, opts, context, onError]);

  // Once a result exists it follows the options and the texts.
  useEffect(() => { if (res && (a.text || b.text)) { const t = setTimeout(run, 150); return () => clearTimeout(t); } }, [opts, context, a.text, b.text]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { if (preset) setRes(undefined); }, [preset?.nonce]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { if (preset && a.text && b.text && !res) void run(); }, [preset?.nonce, a.text, b.text]); // eslint-disable-line react-hooks/exhaustive-deps

  const patch = async () => api.textPatch(a.text, b.text, opts, a.label || "A", b.label || "B", 3);
  const copyPatch = async () => { try { await navigator.clipboard.writeText(await patch()); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch (e) { onError(e); } };
  const savePatch = async () => { try { const p = await api.pickSavePath("changes.patch"); if (!p) return; await api.textWriteFile(p, await patch()); setSaved(p); } catch (e) { onError(e); } };
  const st = res?.stats;
  const lenient = Object.values(opts).some(Boolean);

  return (
    <div className="cmp-pane">
      <div className="cmp-srcs">
        <SourcePicker api={api} conns={conns} sheets={sheets} title="A" value={a} onChange={setA} onError={onError} preset={preset?.a} />
        <SourcePicker api={api} conns={conns} sheets={sheets} title="B" value={b} onChange={setB} onError={onError} preset={preset?.b} />
      </div>
      <div className="opt-row cmp-opts">
        {OPTS.map(([k, l]) => <label key={k} className="check"><input type="checkbox" checked={opts[k]} onChange={(e) => setOpts({ ...opts, [k]: e.target.checked })} /> {l}</label>)}
        <span className="spacer" />
        <div className="segmented tight" role="radiogroup" aria-label="View">
          <button role="radio" aria-checked={view === "side"} className={view === "side" ? "on" : ""} onClick={() => setView("side")}>Side by side</button>
          <button role="radio" aria-checked={view === "unified"} className={view === "unified" ? "on" : ""} onClick={() => setView("unified")}>Unified</button>
        </div>
        <label>Context<select value={context ?? ""} onChange={(e) => setContext(e.target.value === "" ? null : Number(e.target.value))} aria-label="Context lines"><option value={3}>3 lines</option><option value={10}>10 lines</option><option value="">all lines</option></select></label>
        <button className="primary" disabled={busy || (!a.text && !b.text)} onClick={run}>Compare</button>
      </div>
      {st && (
        <div className={`cmp-stats ${st.identical ? "same" : ""}`} role="status" aria-label="Result summary">
          {st.identical
            ? <b>{st.byte_identical ? "Identical." : `Identical${lenient ? " after ignoring the selected differences" : ""}.`}</b>
            : <><b>{st.changed + st.inserted + st.deleted} line{st.changed + st.inserted + st.deleted === 1 ? "" : "s"} differ</b> — {st.changed} changed, {st.inserted} added in B, {st.deleted} missing in B · {st.equal} unchanged</>}
          <span className="spacer" />
          {!st.identical && <><button className="ghost" onClick={copyPatch}>{copied ? "Copied" : "Copy patch"}</button><button className="ghost" onClick={savePatch}>Save patch…</button></>}
        </div>
      )}
      {saved && <div className="ok-box small">Saved {saved}</div>}
      <div className="cmp-result">{res ? (res.stats.identical ? <div className="muted pad">Nothing to show: the two texts are the same.</div> : <DiffView rows={res.rows} mode={view} />) : <div className="muted pad">Choose a source for A and B, then Compare.</div>}</div>
    </div>
  );
}
