import { useEffect, useRef, useState } from "react";
import type { Api } from "../api";
import { errText } from "../hooks";
import { Icon } from "../ui-icons";
import type { HistoryHit, Hit } from "../types";
import { timeAgo } from "../format";
import type { ConnState } from "./Explorer";

export interface ConnHit extends Hit { connId: string; connName: string; env: string }

const ICON: Record<string, string> = { table: "▦", view: "◫", column: "▤", source: "{ }" };

/** Render FTS snippet markers `[word]` as <mark>. */
function Snippet({ text }: { text: string }) {
  return <>{text.split(/(\[[^\]]*\])/).map((p, i) => (p.startsWith("[") && p.endsWith("]") ? <mark key={i}>{p.slice(1, -1)}</mark> : <span key={i}>{p}</span>))}</>;
}

export function SearchPalette({ api, conns, activeConnId, onPick, onPickHistory, onClose }: { api: Api; conns: ConnState[]; activeConnId?: string; onPick(h: ConnHit): void; onPickHistory(h: HistoryHit): void; onClose(): void }) {
  const [q, setQ] = useState("");
  const [all, setAll] = useState(true);
  const [hits, setHits] = useState<ConnHit[]>([]);
  const [past, setPast] = useState<HistoryHit[]>([]);
  const [sel, setSel] = useState(0);
  const [err, setErr] = useState("");
  const seq = useRef(0);

  useEffect(() => {
    const id = ++seq.current;
    const t = setTimeout(async () => {
      const targets = all ? conns : conns.filter((c) => c.id === activeConnId);
      try {
        const per = await Promise.all(targets.map(async (c) => (await api.searchObjects(c.id, q, null, [], 40)).map((h): ConnHit => ({ ...h, connId: c.id, connName: c.profile.name, env: c.profile.environment }))));
        const old = q.trim() ? await api.historySearch({ text: q, only_ok: true, limit: 5, conn_key: all ? null : conns.find((c) => c.id === activeConnId)?.key ?? null }).catch(() => []) : [];
        if (id === seq.current) { setHits(per.flat().sort((a, b) => a.score - b.score).slice(0, 60)); setPast(old); setSel(0); setErr(""); }
      } catch (e) { if (id === seq.current) setErr(errText(e)); }
    }, 100);
    return () => clearTimeout(t);
  }, [q, all, conns, activeConnId, api]);

  return (
    <div className="modal-back top" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-label="Search">
        <div className="palette-input">
          <Icon name="search" size={16} />
          <input
            autoFocus placeholder="Search tables, columns, comments and PL/SQL source…"
            value={q} onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") onClose();
              else if (e.key === "ArrowDown") { e.preventDefault(); setSel((s) => Math.min(s + 1, hits.length + past.length - 1)); }
              else if (e.key === "ArrowUp") { e.preventDefault(); setSel((s) => Math.max(s - 1, 0)); }
              else if (e.key === "Enter") { if (sel < hits.length) onPick(hits[sel]); else if (past[sel - hits.length]) onPickHistory(past[sel - hits.length]); }
            }}
          />
          <kbd>esc</kbd>
        </div>
        <label className="palette-scope"><input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} /> Search all {conns.length} connection{conns.length === 1 ? "" : "s"}</label>
        {err && <div className="error">{err}</div>}
        <div className="hits">
          {hits.length === 0 && past.length === 0 && q.trim() !== "" && <div className="muted pad">No matches</div>}
          {hits.map((h, i) => (
            <div key={`${h.connId}${h.kind}${h.owner}${h.parent}${h.name}${h.line}`} className={`hit ${i === sel ? "sel" : ""}`} onMouseEnter={() => setSel(i)} onClick={() => onPick(h)}>
              <span className="hk">{ICON[h.kind] ?? "•"}</span>
              <span className="hn">
                <span><strong>{h.name}</strong> <span className="muted">{h.kind === "column" ? `in ${h.parent}` : h.kind === "source" ? `${h.parent.toLowerCase()} · line ${h.line}` : h.kind} · {h.owner}</span></span>
                {h.snippet && <span className="small muted snippet"><Snippet text={h.snippet} /></span>}
              </span>
              <span className={`chip env-${h.env}`}>{h.connName}</span>
            </div>
          ))}
          {past.length > 0 && <div className="palette-section">From your history</div>}
          {past.map((h, j) => {
            const i = hits.length + j;
            return (
              <div key={h.hash} className={`hit ${i === sel ? "sel" : ""}`} onMouseEnter={() => setSel(i)} onClick={() => onPickHistory(h)}>
                <span className="hk">⟲</span>
                <span className="hn">
                  <span className="snippet">{h.sql.replace(/\s+/g, " ").slice(0, 110)}</span>
                  <span className="small muted">ran {h.runs}× · last {timeAgo(h.last_run)} · {h.last_conn_name}</span>
                </span>
                <span className="chip">{h.kind}</span>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
