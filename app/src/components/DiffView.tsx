import { useMemo, useState } from "react";
import type { DiffRow, DiffSpan } from "../types";

const CAP = 4000;

function Spans({ spans, text, cls }: { spans: DiffSpan[]; text: string | null; cls: string }) {
  if (!spans.length) return <>{text ?? ""}</>;
  return <>{spans.map((s, i) => (s.changed ? <mark key={i} className={cls}>{s.text}</mark> : <span key={i}>{s.text}</span>))}</>;
}

/** Side-by-side or unified view of a text diff. Long results are cut at 4,000 rows with a way to show all. */
export function DiffView({ rows, mode }: { rows: DiffRow[]; mode: "side" | "unified" }) {
  const [all, setAll] = useState(false);
  const shown = useMemo(() => (all ? rows : rows.slice(0, CAP)), [rows, all]);
  return (
    <div className="diff" role="table" aria-label="Differences">
      {shown.map((r, i) => {
        if (r.kind === "skip") return <div key={i} className="d-skip" role="row">⋯ {r.skipped.toLocaleString()} unchanged line{r.skipped === 1 ? "" : "s"}</div>;
        if (mode === "unified") {
          const lines: [string, string, number | null, string | null, DiffSpan[], string][] =
            r.kind === "equal" ? [["equal", " ", r.a_no, r.a, [], "d-eq"]]
            : r.kind === "delete" ? [["delete", "-", r.a_no, r.a, [], "d-del"]]
            : r.kind === "insert" ? [["insert", "+", r.b_no, r.b, [], "d-ins"]]
            : [["delete", "-", r.a_no, r.a, r.a_spans, "d-del"], ["insert", "+", r.b_no, r.b, r.b_spans, "d-ins"]];
          return lines.map(([k, sign, no, text, sp, cls], j) => (
            <div key={`${i}-${j}`} className={`d-row u ${cls}`} role="row" data-kind={k}>
              <span className="d-no">{no ?? ""}</span><span className="d-sign">{sign}</span>
              <code className="d-text"><Spans spans={sp} text={text} cls={k === "delete" ? "del" : "ins"} /></code>
            </div>
          ));
        }
        return (
          <div key={i} className={`d-row s k-${r.kind}`} role="row" data-kind={r.kind}>
            <span className="d-no">{r.a_no ?? ""}</span>
            <code className={`d-text ${r.kind === "delete" || r.kind === "change" ? "d-del" : r.kind === "insert" ? "d-void" : ""}`}><Spans spans={r.a_spans} text={r.a} cls="del" /></code>
            <span className="d-no">{r.b_no ?? ""}</span>
            <code className={`d-text ${r.kind === "insert" || r.kind === "change" ? "d-ins" : r.kind === "delete" ? "d-void" : ""}`}><Spans spans={r.b_spans} text={r.b} cls="ins" /></code>
          </div>
        );
      })}
      {!all && rows.length > CAP && <div className="d-skip" role="row">Showing the first {CAP.toLocaleString()} of {rows.length.toLocaleString()} rows. <button className="ghost" onClick={() => setAll(true)}>Show all</button></div>}
    </div>
  );
}
