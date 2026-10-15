import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../api";
import { errText, useAsync } from "../hooks";
import { fmtBytes, timeAgo } from "../format";
import type { Direction, DossierOptions, KbExported, KbPreview } from "../types";

const DIRS: [Direction, string][] = [["uses", "What it uses"], ["used_by", "What uses it"], ["both", "Both"]];

export function KnowledgeDialog({ api, conn, owner, focus, label, onClose, onError }: {
  api: Api; conn: string; owner: string; focus: string; label: string; onClose(): void; onError(e: unknown): void;
}) {
  const status = useAsync(() => api.kbStatus(conn, owner), [api, conn, owner]);
  const [building, setBuilding] = useState(false);
  const [direction, setDirection] = useState<Direction>("both");
  const [hops, setHops] = useState(1);
  const [source, setSource] = useState(true);
  const [cap, setCap] = useState(120);
  const [preview, setPreview] = useState<KbPreview>();
  const [previewErr, setPreviewErr] = useState("");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<KbExported>();
  const [copied, setCopied] = useState(false);
  const seq = useRef(0);
  const kb = status.data;

  const options = useCallback((): DossierOptions => ({ focus, direction, hops, include_source: source, max_member_lines: cap }), [focus, direction, hops, source, cap]);

  useEffect(() => {
    if (!kb) { setPreview(undefined); return; }
    const id = ++seq.current;
    setBusy(true);
    const t = setTimeout(() => {
      api.kbPreview(conn, owner, options()).then(
        (p) => { if (id === seq.current) { setPreview(p); setPreviewErr(""); } },
        (e) => { if (id === seq.current) { setPreview(undefined); setPreviewErr(errText(e)); } },
      ).finally(() => { if (id === seq.current) setBusy(false); });
    }, 250);
    return () => clearTimeout(t);
  }, [api, conn, owner, kb, options]);

  const build = async () => {
    setBuilding(true);
    try { await api.kbBuild(conn, owner); status.reload(); } catch (e) { onError(e); } finally { setBuilding(false); }
  };
  const name = focus.split(":")[1]?.split(".").pop()?.toLowerCase() ?? "export";
  const save = async (zip: boolean) => {
    try {
      const path = await api.pickSavePath(`${name}-knowledge.${zip ? "zip" : "md"}`);
      if (!path) return;
      setBusy(true);
      setDone(await api.kbExport(conn, owner, options(), path, zip));
    } catch (e) { onError(e); } finally { setBusy(false); }
  };
  const copy = async () => {
    if (!preview) return;
    try { await navigator.clipboard.writeText(preview.markdown); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch (e) { onError(e); }
  };

  return (
    <div className="modal-back">
      <div className="modal wide kb" role="dialog" aria-label="Knowledge export">
        <h3>Knowledge export <span className="muted">{label}</span></h3>

        {!kb && !status.loading && (
          <div className="warn-box">
            <b>The knowledge base for {owner} has not been built yet.</b> It reads every table, column, key, index, trigger, package and
            dependency once (and the package bodies) and keeps the result in your local file. Nothing leaves your machine.
            <div className="actions left"><button className="primary" disabled={building} onClick={build}>{building ? "Building…" : "Build knowledge base"}</button></div>
          </div>
        )}
        {kb && (
          <div className="kb-status" aria-label="Knowledge base status">
            <span>Built {timeAgo(kb.taken_at)}: {kb.tables} tables, {kb.views} views, {kb.packages} packages ({kb.members} members), {kb.triggers} triggers, {kb.indexes} indexes, {kb.nodes} objects, {kb.edges} links
              ({kb.exact_links} exact, {kb.heuristic_links} heuristic).</span>
            <button className="ghost" disabled={building} onClick={build}>{building ? "Rebuilding…" : "Rebuild"}</button>
          </div>
        )}
        {kb && kb.coverage.gaps.length > 0 && (
          <details className="kb-gaps">
            <summary>What the knowledge base cannot see ({kb.coverage.gaps.filter((g) => g.severity === "warning").length} warnings, {kb.coverage.gaps.filter((g) => g.severity === "info").length} notes)</summary>
            <ul>{kb.coverage.gaps.map((g, i) => <li key={i} className={g.severity}><b>{g.area}:</b> {g.message}</li>)}</ul>
          </details>
        )}

        {kb && (
          <>
            <div className="opt-row">
              <div className="segmented tight" role="radiogroup" aria-label="Direction">
                {DIRS.map(([d, l]) => <button key={d} role="radio" aria-checked={direction === d} className={direction === d ? "on" : ""} onClick={() => setDirection(d)}>{l}</button>)}
              </div>
              <label>Hops<select value={hops} onChange={(e) => setHops(Number(e.target.value))} aria-label="Hops">{[1, 2, 3, 4].map((n) => <option key={n}>{n}</option>)}</select></label>
              <label className="check"><input type="checkbox" checked={source} onChange={(e) => setSource(e.target.checked)} /> Include source code and DDL</label>
              {source && <label>Code per member<select value={cap} onChange={(e) => setCap(Number(e.target.value))} aria-label="Lines per member">
                <option value={60}>60 lines</option><option value={120}>120 lines</option><option value={400}>400 lines</option><option value={0}>no limit</option></select></label>}
            </div>
            <div className="kb-meta small muted" aria-live="polite">
              {busy && !preview ? "Building preview…" : preview ? <>
                {preview.files.filter((f) => f.endsWith(".md")).length - 1} object file(s) + index · about {preview.tokens_estimate.toLocaleString()} tokens
                {preview.truncated && " · neighbourhood truncated (lower the hops)"}
                {" · "}row data is never included; passwords in source are masked.
              </> : null}
            </div>
            {previewErr && <div className="error" role="alert">{previewErr}</div>}
            {preview && preview.missing.length > 0 && <div className="warn-box small">Could not read the text of: {preview.missing.join("; ")}</div>}
            {preview && <pre className="kb-preview" aria-label="Markdown preview" tabIndex={0}>{preview.markdown}{preview.preview_cut ? "\n\n… preview cut; the download is complete." : ""}</pre>}
            {done && <div className="ok-box" role="status">Saved {done.files} file(s), {fmtBytes(done.bytes)}, ≈ {done.tokens_estimate.toLocaleString()} tokens: <span className="mono">{done.path}</span></div>}
          </>
        )}
        <div className="actions">
          <button onClick={onClose}>Close</button>
          {kb && <button disabled={!preview} onClick={copy}>{copied ? "Copied" : "Copy markdown"}</button>}
          {kb && <button disabled={!preview || busy} onClick={() => save(true)} title="One .md per object, an index, graph.json and manifest.json">Download .zip</button>}
          {kb && <button className="primary" disabled={!preview || busy} onClick={() => save(false)} title="Everything in one markdown file">Download .md</button>}
        </div>
      </div>
    </div>
  );
}
