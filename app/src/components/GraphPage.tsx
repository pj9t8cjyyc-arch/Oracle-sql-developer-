import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Api } from "../api";
import { errText, useAsync } from "../hooks";
import type { Direction, GNode, SNode, Subgraph } from "../types";

const KIND_COLOR: Record<string, string> = {
  TABLE: "#2f6bed", VIEW: "#1b8f78", "MATERIALIZED VIEW": "#1b8f78", PACKAGE: "#7c5cd6", PROCEDURE: "#9a6bd6", FUNCTION: "#9a6bd6",
  TRIGGER: "#e0701f", SEQUENCE: "#d6336c", TYPE: "#7b8696", SYNONYM: "#7b8696",
};
const KINDS = ["TABLE", "VIEW", "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "SEQUENCE", "TYPE", "SYNONYM"];
const COLW = 250, ROWH = 46, BOXW = 196, BOXH = 32, PAD = 24;
const DIRS: [Direction, string][] = [["uses", "What it uses"], ["used_by", "What uses it"], ["both", "Both"]];

export function GraphPage({ api, conn, owner, focus: focusReq, onOpen, onError }: {
  api: Api; conn: string; owner: string; focus?: string;
  onOpen(kind: string, owner: string, name: string): void; onError(e: unknown): void;
}) {
  const info = useAsync(() => api.graphInfo(conn, owner, false), [api, conn, owner]);
  const [focus, setFocus] = useState<string>();
  const [dir, setDir] = useState<Direction>("used_by");
  const [hops, setHops] = useState(2);
  const [kinds, setKinds] = useState<string[]>([]);
  const [sub, setSub] = useState<Subgraph>();
  const [err, setErr] = useState("");
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<GNode[]>([]);
  const [pickerOpen, setPickerOpen] = useState(false);
  const seq = useRef(0);

  useEffect(() => { if (focusReq) setFocus(focusReq); }, [focusReq]);

  useEffect(() => {
    if (!focus) { setSub(undefined); return; }
    const id = ++seq.current;
    api.graphFocus(conn, owner, { id: focus, direction: dir, hops, kinds: kinds.length ? kinds : null }).then(
      (s) => { if (id === seq.current) { setSub(s); setErr(""); } },
      (e) => { if (id === seq.current) { setSub(undefined); setErr(errText(e)); } },
    );
  }, [api, conn, owner, focus, dir, hops, kinds]);

  useEffect(() => {
    if (!pickerOpen) return;
    let live = true;
    const t = setTimeout(() => api.graphSearch(conn, owner, q).then((h) => { if (live) setHits(h); }, onError), 120);
    return () => { live = false; clearTimeout(t); };
  }, [api, conn, owner, q, pickerOpen, onError]);

  const layout = useMemo(() => {
    if (!sub) return undefined;
    const cols = new Map<number, SNode[]>();
    for (const n of sub.nodes) cols.set(n.depth, [...(cols.get(n.depth) ?? []), n]);
    const depths = [...cols.keys()].sort((a, b) => a - b);
    // Used-by on the left, uses on the right: arrows read left to right as "uses".
    const minD = depths[0] ?? 0;
    const rows = Math.max(...[...cols.values()].map((c) => c.length), 1);
    const pos = new Map<string, { x: number; y: number }>();
    for (const [d, ns] of cols) {
      const off = ((rows - ns.length) * ROWH) / 2;
      ns.forEach((n, i) => pos.set(n.id, { x: PAD + (d - minD) * COLW, y: PAD + off + i * ROWH }));
    }
    return { pos, w: PAD * 2 + (depths.length ? (depths[depths.length - 1] - minD) * COLW : 0) + BOXW, h: PAD * 2 + rows * ROWH };
  }, [sub]);

  const toggleKind = (k: string) => setKinds((ks) => (ks.includes(k) ? ks.filter((x) => x !== k) : [...ks, k]));
  const focusNode = sub?.nodes.find((n) => n.id === sub.focus);
  const choose = useCallback((n: GNode) => { setFocus(n.id); setPickerOpen(false); setQ(""); }, []);

  return (
    <div className="gr-page">
      <div className="gr-bar">
        <div className="gr-pick">
          <input value={q} placeholder={focusNode ? `${focusNode.kind.toLowerCase()} ${focusNode.name}` : "Pick an object to start…"} aria-label="Focus object"
            onFocus={() => setPickerOpen(true)} onChange={(e) => { setQ(e.target.value); setPickerOpen(true); }} onBlur={() => setTimeout(() => setPickerOpen(false), 150)} />
          {pickerOpen && hits.length > 0 && (
            <ul className="gr-hits" role="listbox">
              {hits.map((h) => (
                <li key={h.id} role="option" aria-selected={false}><button onMouseDown={(e) => e.preventDefault()} onClick={() => choose(h)}>
                  <span className="gr-dot" style={{ background: KIND_COLOR[h.kind] ?? "#7b8696" }} /> {h.name} <span className="muted small">{h.kind.toLowerCase()}</span></button></li>
              ))}
            </ul>
          )}
        </div>
        <div className="segmented tight" role="radiogroup" aria-label="Direction">
          {DIRS.map(([d, l]) => <button key={d} role="radio" aria-checked={dir === d} className={dir === d ? "on" : ""} onClick={() => setDir(d)}>{l}</button>)}
        </div>
        <label className="inline small">Hops
          <select value={hops} onChange={(e) => setHops(Number(e.target.value))} aria-label="Hops">{[1, 2, 3, 4, 5, 6].map((n) => <option key={n}>{n}</option>)}</select></label>
        <div className="gr-kinds" aria-label="Show only">
          {KINDS.filter((k) => !info.data || info.data.kinds[k]).map((k) => (
            <button key={k} className={`chipbtn ${kinds.includes(k) ? "on" : ""}`} aria-pressed={kinds.includes(k)} onClick={() => toggleKind(k)}>
              <span className="gr-dot" style={{ background: KIND_COLOR[k] }} />{k.toLowerCase()}
            </button>
          ))}
        </div>
      </div>

      {info.data && <div className="gr-meta muted small">{owner}: {info.data.nodes} objects, {info.data.edges} links. Arrows point from the object that uses to the object it uses; dashed = foreign key.
        {" "}Click an object to re-centre, double-click to open it. {kinds.length > 0 && "Hidden kinds are not followed."}</div>}
      {err && <div className="error pad" role="alert">{err}</div>}
      {sub?.truncated && <div className="warn-box small">Showing the first {sub.nodes.length} objects. Lower the hop count or filter by kind to see the rest.</div>}

      <div className="gr-canvas">
        {!focus && <div className="muted pad">Choose an object above to see what it uses and what uses it. Try a table to answer "what breaks if I change this?".</div>}
        {sub && layout && (
          <svg width={layout.w} height={layout.h} role="img" aria-label="Dependency graph">
            <defs>
              <marker id="arr" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto"><path d="M0 0 8 4 0 8z" fill="var(--text-3)" /></marker>
            </defs>
            {sub.edges.map((e, i) => {
              const a = layout.pos.get(e.from), b = layout.pos.get(e.to);
              if (!a || !b) return null;
              const forward = b.x >= a.x;
              const x1 = forward ? a.x + BOXW : a.x, x2 = forward ? b.x : b.x + BOXW;
              const y1 = a.y + BOXH / 2, y2 = b.y + BOXH / 2;
              const same = a.x === b.x;
              const d = same
                ? `M${a.x} ${y1} C${a.x - 50} ${y1}, ${a.x - 50} ${y2}, ${b.x} ${y2}`
                : `M${x1} ${y1} C${(x1 + x2) / 2} ${y1}, ${(x1 + x2) / 2} ${y2}, ${x2} ${y2}`;
              const touches = e.from === sub.focus || e.to === sub.focus;
              return <path key={i} d={d} fill="none" stroke={touches ? "var(--accent)" : "var(--line-strong)"} strokeWidth={touches ? 1.6 : 1.1} strokeDasharray={e.kind === "fk" ? "5 4" : undefined} markerEnd="url(#arr)" />;
            })}
            {sub.nodes.map((n) => {
              const p = layout.pos.get(n.id)!;
              const isFocus = n.id === sub.focus;
              return (
                <g key={n.id} transform={`translate(${p.x},${p.y})`} className={`gr-node ${isFocus ? "focus" : ""}`} tabIndex={0} role="button" aria-label={`${n.kind} ${n.name}`}
                  onClick={() => setFocus(n.id)} onDoubleClick={() => onOpen(n.kind, n.owner, n.name)} onKeyDown={(e) => { if (e.key === "Enter") setFocus(n.id); }}>
                  <title>{n.kind.toLowerCase()} {n.owner}.{n.name}{n.more ? ` — ${n.more} more not shown` : ""}</title>
                  <rect width={BOXW} height={BOXH} rx={7} />
                  <rect width={5} height={BOXH} rx={2} fill={KIND_COLOR[n.kind] ?? "#7b8696"} />
                  <text x={14} y={14} className="gr-name">{n.name.length > 24 ? `${n.name.slice(0, 23)}…` : n.name}</text>
                  <text x={14} y={26} className="gr-kind">{n.kind.toLowerCase()}{n.owner !== owner ? ` · ${n.owner}` : ""}</text>
                  {n.more > 0 && <text x={BOXW - 8} y={20} textAnchor="end" className="gr-more">+{n.more}</text>}
                </g>
              );
            })}
          </svg>
        )}
      </div>
    </div>
  );
}
