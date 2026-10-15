import type { CompileReport } from "../types";

export interface OutputBlock { title: string; lines: string[]; compile: CompileReport | null }

/** What PL/SQL wrote with DBMS_OUTPUT, and why a stored unit does not compile. */
export function OutputPanel({ blocks, onClose }: { blocks: OutputBlock[]; onClose(): void }) {
  return (
    <div className="out-panel" data-testid="output">
      <div className="plan-head"><b>Output</b><span className="muted">{blocks.length} statement{blocks.length === 1 ? "" : "s"}</span><span className="grow" /><button className="ghost" onClick={onClose} aria-label="Clear output">✕</button></div>
      {blocks.map((b, i) => (
        <div key={i} className="out-block">
          <div className="out-title">{b.title}</div>
          {b.compile && (
            <div className={`out-compile ${b.compile.has_errors ? "bad" : "warn"}`}>
              <b>{b.compile.object}</b> {b.compile.has_errors ? "was created but does not compile" : "compiled with warnings"}
              <table><tbody>
                {b.compile.issues.map((x, k) => <tr key={k}><td className="num">{x.line}:{x.column}</td><td>{x.severity}</td><td>{x.text}</td></tr>)}
              </tbody></table>
              <div className="muted">Line and column count from the start of the statement as sent (line 1 is the CREATE line).</div>
            </div>
          )}
          {b.lines.length > 0 && <pre className="out-lines">{b.lines.join("\n")}</pre>}
        </div>
      ))}
    </div>
  );
}
