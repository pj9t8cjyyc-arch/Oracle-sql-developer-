import { useEffect, useRef, useState } from "react";
import type { Column, Row } from "../types";

const ROW_H = 24;
const OVERSCAN = 20;

interface Props {
  columns: Column[];
  rows: Row[];
  /** Called when the user scrolls within ~1 page of the end. */
  onNearEnd(): void;
}

/** Fixed-row-height virtual grid: DOM size is independent of row count. */
export function ResultGrid({ columns, rows, onNearEnd }: Props) {
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
  const template = `56px repeat(${columns.length}, minmax(120px, 1fr))`;

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
      <div className="grid-head" style={{ gridTemplateColumns: template }}>
        <div className="rownum">#</div>
        {columns.map((c) => (
          <div key={c.name} title={`${c.name} ${c.data_type}`}>{c.name}</div>
        ))}
      </div>
      <div style={{ height: rows.length * ROW_H, position: "relative" }}>
        {rows.slice(first, last).map((r, k) => {
          const i = first + k;
          return (
            <div key={i} className="grid-row" style={{ top: i * ROW_H, height: ROW_H, gridTemplateColumns: template }}>
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
