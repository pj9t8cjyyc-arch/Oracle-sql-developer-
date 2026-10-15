import { useState } from "react";
import type { Plan } from "../types";

const fmt = (n: number | null) => (n === null ? "" : n >= 1e9 ? `${(n / 1e9).toFixed(1)}G` : n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}K` : String(n));

/** The execution plan as an indented tree with a cost bar per step, and what to look at first. */
export function PlanPanel({ plan, onClose }: { plan: Plan; onClose(): void }) {
  const [sel, setSel] = useState<number | null>(null);
  const [raw, setRaw] = useState(false);
  const picked = plan.nodes.find((n) => n.id === sel);
  const marked = new Set(plan.findings.filter((f) => sel !== null && f.steps.includes(sel)).flatMap((f) => f.steps));
  return (
    <div className="plan" data-testid="plan">
      <div className="plan-head">
        <b>Execution plan</b>
        <span className="muted">total cost {plan.total_cost ?? "?"} · {plan.nodes.length} steps · nothing was run</span>
        <span className="grow" />
        <button className="ghost" onClick={() => setRaw(!raw)}>{raw ? "Tree" : "Raw text"}</button>
        <button className="ghost" onClick={onClose} aria-label="Close plan">✕</button>
      </div>
      {plan.findings.length > 0 && (
        <ul className="plan-findings">
          {plan.findings.map((f, i) => (
            <li key={i} className={`sev-${f.severity}`} onClick={() => setSel(f.steps[0] ?? null)}>
              <b>{f.title}</b> <span className="muted">{f.steps.length ? `step ${f.steps.join(", ")}` : ""}</span>
              <div>{f.detail}</div>
              {f.suggestion && <div className="suggest">→ {f.suggestion}</div>}
            </li>
          ))}
        </ul>
      )}
      {raw ? <pre className="plan-raw">{plan.text}</pre> : (
        <table className="plan-table">
          <thead><tr><th>Step</th><th>Operation</th><th>Object</th><th>Rows</th><th>Cost</th><th className="bar">Share</th></tr></thead>
          <tbody>
            {plan.nodes.map((n) => (
              <tr key={n.id} className={`${n.hot ? "hot" : ""} ${sel === n.id ? "sel" : ""} ${marked.has(n.id) ? "marked" : ""}`} onClick={() => setSel(n.id === sel ? null : n.id)}>
                <td>{n.id}</td>
                <td style={{ paddingLeft: 8 + n.depth * 16 }}>{n.operation}</td>
                <td>{n.object_name ? `${n.object_owner ?? ""}${n.object_owner ? "." : ""}${n.object_name}` : ""}</td>
                <td className="num">{fmt(n.rows)}</td>
                <td className="num">{fmt(n.cost)}</td>
                <td className="bar"><span style={{ width: `${Math.min(100, n.cost_pct ?? 0)}%` }} />{n.cost_pct ? `${Math.round(n.cost_pct)}%` : ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {picked && (picked.access || picked.filter) && (
        <div className="plan-pred">
          {picked.access && <div><b>access:</b> <code>{picked.access}</code></div>}
          {picked.filter && <div><b>filter:</b> <code>{picked.filter}</code></div>}
        </div>
      )}
      {plan.notes.length > 0 && <div className="plan-notes muted">{plan.notes.join(" · ")}</div>}
    </div>
  );
}
