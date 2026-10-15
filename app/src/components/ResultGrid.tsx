import { useEffect, useRef, useState } from "react";
import type { Column, Row } from "../types";
import type { Badge } from "../badges";

const ROW_H = 24;
const OVERSCAN = 20;
const COL_W = 160;

interface Props {
  columns: Column[];
  rows: Row[];
  /** Called when the user scrolls within ~1 page of the end. */
  onNearEnd(): void;
  badges: Record<string, Badge[]>;
  selectedColumn?: string;
  onSelectColumn(name: string): void;
}

/** Fixed-row-height virtual grid: DOM size is independent of row count. */
export function ResultGrid({ columns, rows, onNearEnd, badges, selectedColumn, onSelectColumn }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState(0);
  const [height, setHeight] = useState(400);

  useEffect(() => {
    const el = box.current!;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const first = Math.max(0, Math.floor(top / ROW_H) - OVERSCAN);
  const last = Math.min(rows.length, Math.ceil((top + height) / ROW_H) + OVERSCAN);
  // Same template and explicit minimum width for header and every row, so columns line up
  // even when cell content is longer than the column.
  const widths = columns.map((c) => Math.min(340, Math.max(COL_W, Math.round(c.name.length * 8.4 + (badges[c.name]?.length ?? 0) * 30 + 28))));
  const template = `56px ${widths.map((w) => `minmax(${w}px, 1fr)`).join(" ")}`;
  const minWidth = 56 + widths.reduce((a, b) => a + b, 0);

  return (
    <div
      className="grid"
      ref={box}
      onScroll={(e) => {
        const el = e.currentTarget;
        setTop(el.scrollTop);
        if (el.scrollTop + el.clientHeight > el.scrollHeight - el.clientHeight) onNearEnd();
      }}
    >
      <div className="grid-head" style={{ gridTemplateColumns: template, minWidth }}>
        <div className="rownum">#</div>
        {columns.map((c) => (
          <div key={c.name} className={`hcell ${selectedColumn === c.name ? "sel" : ""}`} title={`${c.name} ${c.data_type}`} onClick={() => onSelectColumn(c.name)}>
            <span className="hname">{c.name}</span>
            {(badges[c.name] ?? []).map((b, i) => <span key={i} className={`badge ${b.tone}`} title={b.title}>{b.text}</span>)}
          </div>
        ))}
      </div>
      <div style={{ height: rows.length * ROW_H, position: "relative", minWidth }}>
        {rows.slice(first, last).map((r, k) => {
          const i = first + k;
          return (
            <div key={i} className="grid-row" style={{ top: i * ROW_H, height: ROW_H, gridTemplateColumns: template, minWidth }}>
              <div className="rownum">{i + 1}</div>
              {r.map((c, j) => (
                <div key={j} className={c === null ? "null" : ""} title={c ?? "NULL"}>{c === null ? "(null)" : c}</div>
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}
