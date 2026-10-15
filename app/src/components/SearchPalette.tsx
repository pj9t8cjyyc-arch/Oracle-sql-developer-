import { useEffect, useRef, useState } from "react";
import type { Api } from "../api";
import type { Hit } from "../types";

const ICON: Record<string, string> = { table: "▦", view: "◫", column: "▤", source: "{ }" };

/** Render FTS snippet markers `[word]` as <mark>. */
function Snippet({ text }: { text: string }) {
  return <>{text.split(/(\[[^\]]*\])/).map((p, i) => (p.startsWith("[") && p.endsWith("]") ? <mark key={i}>{p.slice(1, -1)}</mark> : <span key={i}>{p}</span>))}</>;
}

export function SearchPalette({ api, owner, onPick, onClose }: { api: Api; owner: string; onPick(h: Hit): void; onClose(): void }) {
  const [q, setQ] = useState("");
  const [allSchemas, setAll] = useState(false);
  const [hits, setHits] = useState<Hit[]>([]);
  const [sel, setSel] = useState(0);
  const seq = useRef(0);

  useEffect(() => {
    const id = ++seq.current;
    const t = setTimeout(() => {
      api.searchObjects(q, allSchemas ? null : owner, [], 40).then((h) => { if (id === seq.current) { setHits(h); setSel(0); } }).catch(() => {});
    }, 100);
    return () => clearTimeout(t);
  }, [q, allSchemas, owner, api]);

  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-label="Search">
        <input
          autoFocus placeholder="Search tables, columns, comments, PL/SQL source…  (↑↓ Enter, Esc)"
          value={q} onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
            else if (e.key === "ArrowDown") { e.preventDefault(); setSel((s) => Math.min(s + 1, hits.length - 1)); }
            else if (e.key === "ArrowUp") { e.preventDefault(); setSel((s) => Math.max(s - 1, 0)); }
            else if (e.key === "Enter" && hits[sel]) onPick(hits[sel]);
          }}
        />
        <label className="muted small"><input type="checkbox" checked={allSchemas} onChange={(e) => setAll(e.target.checked)} /> all schemas (default: {owner})</label>
        <div className="hits">
          {hits.length === 0 && q.trim() !== "" && <div className="muted pad">No matches</div>}
          {hits.map((h, i) => (
            <div key={`${h.kind}${h.owner}${h.parent}${h.name}${h.line}`} className={`hit ${i === sel ? "sel" : ""}`} onMouseEnter={() => setSel(i)} onClick={() => onPick(h)}>
              <span className="hk">{ICON[h.kind] ?? "•"}</span>
              <span className="hn"><strong>{h.name}</strong> <span className="muted">{h.kind === "column" ? `in ${h.parent}` : h.kind === "source" ? `${h.parent.toLowerCase()} · line ${h.line}` : ""} · {h.owner}</span>
                {h.snippet && <div className="small muted"><Snippet text={h.snippet} /></div>}
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
