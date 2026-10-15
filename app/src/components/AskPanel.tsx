import { useEffect, useRef, useState } from "react";
import type { Api } from "../api";
import { chatWith } from "../ai-client";
import { errText } from "../hooks";
import type { AskContext, AskMode, ChatMessage, ContextPolicy, SqlIssue } from "../types";

const MODES: [AskMode, string][] = [["write_sql", "Write SQL"], ["ask", "Ask"], ["explain", "Explain"], ["fix", "Fix error"]];
const POLICY_KEY = "ora.ask.policy";

function loadPolicy(): ContextPolicy {
  const d: ContextPolicy = { include_comments: true, include_history: true, exclude: [], max_tokens: 6000, max_objects: 25 };
  try { return { ...d, ...JSON.parse(localStorage.getItem(POLICY_KEY) ?? "{}") }; } catch { return d; }
}

interface Turn { q: string; a: string; sql: string[]; issues: SqlIssue[][]; context?: AskContext; error?: string }

/** Questions about the connected schema. Only the part of the schema the question needs is sent, and it is shown here. */
export function AskPanel({ api, conn, owner, getSql, lastError, onInsert, onClose }: {
  api: Api; conn: string; owner: string; getSql(): string; lastError: string; onInsert(sql: string): void; onClose(): void;
}) {
  const [mode, setMode] = useState<AskMode>("write_sql");
  const [text, setText] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const [policy, setPolicy] = useState<ContextPolicy>(loadPolicy);
  const [showCtx, setShowCtx] = useState(false);
  const [preview, setPreview] = useState<AskContext>();
  const cancel = useRef<() => void>();
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => { try { localStorage.setItem(POLICY_KEY, JSON.stringify(policy)); } catch { /* optional */ } }, [policy]);
  useEffect(() => { bottom.current?.scrollIntoView?.({ block: "end" }); }, [turns]);
  useEffect(() => { if (mode === "fix" && lastError && !text) setText("Why did this fail and how do I fix it?"); }, [mode, lastError, text]);

  const input = (history: ChatMessage[]) => ({ mode, question: text.trim(), sql: getSql().trim().slice(0, 8000) || null, error: mode === "fix" ? lastError || null : null, history });

  useEffect(() => {
    if (!showCtx || !text.trim()) { setPreview(undefined); return; }
    const t = setTimeout(() => { api.askContext(conn, owner, input([]), policy).then(setPreview, () => setPreview(undefined)); }, 300);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showCtx, text, mode, policy, conn, owner]);

  const send = async () => {
    if (!text.trim() || busy) return;
    const history: ChatMessage[] = turns.flatMap((t) => t.a ? [{ role: "user" as const, content: t.q }, { role: "assistant" as const, content: t.a }] : []);
    const idx = turns.length;
    setTurns((t) => [...t, { q: text.trim(), a: "", sql: [], issues: [] }]);
    setBusy(true);
    const patch = (p: Partial<Turn>) => setTurns((t) => t.map((x, i) => (i === idx ? { ...x, ...p } : x)));
    try {
      let ctx: AskContext | undefined;
      const run = chatWith(api, async () => { const s = await api.askStart(conn, owner, input(history), policy); ctx = s.context; patch({ context: ctx }); return s.id; }, (d) => setTurns((t) => t.map((x, i) => (i === idx ? { ...x, a: x.a + d } : x))));
      cancel.current = run.cancel;
      setText("");
      const r = await run.promise;
      const sql = await api.askExtractSql(r.text);
      const issues = await Promise.all(sql.map((s) => api.askValidate(conn, owner, s).catch(() => [] as SqlIssue[])));
      patch({ a: r.text, sql, issues });
    } catch (e) { patch({ error: errText(e) }); }
    finally { setBusy(false); cancel.current = undefined; }
  };

  return (
    <aside className="drawer ask" data-testid="ask">
      <div className="ask-head">
        <b>Ask AI</b><span className="muted">about {owner}</span><span className="grow" />
        <button className="ghost" onClick={onClose} aria-label="Close Ask">✕</button>
      </div>
      <div className="ask-modes">{MODES.map(([m, l]) => <button key={m} className={mode === m ? "primary" : ""} onClick={() => setMode(m)}>{l}</button>)}</div>
      <div className="ask-log">
        {turns.length === 0 && <div className="empty-note">Describe what you need, e.g. “employees and their department, highest salary first”. The model only sees the tables your question needs — open “What is sent” to check.</div>}
        {turns.map((t, i) => (
          <div key={i} className="turn">
            <div className="q">{t.q}</div>
            <div className="a">{t.a.split(/```sql\n[\s\S]*?```/).map((part, k, arr) => (
              <span key={k}>{part}{k < arr.length - 1 && t.sql[k] !== undefined && (
                <div className="sqlblock"><pre>{t.sql[k]}</pre>
                  {(t.issues[k] ?? []).map((s, j) => <div key={j} className={`issue ${s.severity}`}>{s.severity === "error" ? "✖" : "⚠"} {s.message}</div>)}
                  <button onClick={() => onInsert(t.sql[k])}>Insert into worksheet</button>
                  <button className="ghost" onClick={() => void navigator.clipboard?.writeText(t.sql[k])}>Copy</button>
                </div>)}</span>
            ))}{busy && i === turns.length - 1 && !t.error && <span className="muted"> …</span>}</div>
            {t.error && <div className="error">{t.error}</div>}
            {t.context && <div className="muted ctx-line">sent {t.context.objects.length} objects · ~{t.context.tokens_estimate} tokens{t.context.excluded ? ` · ${t.context.excluded} hidden` : ""}{t.context.truncated ? " · cut to fit" : ""}</div>}
          </div>
        ))}
        <div ref={bottom} />
      </div>
      <details className="ask-ctx" open={showCtx} onToggle={(e) => setShowCtx((e.target as HTMLDetailsElement).open)}>
        <summary>What is sent</summary>
        <label><input type="checkbox" checked={policy.include_comments} onChange={(e) => setPolicy({ ...policy, include_comments: e.target.checked })} /> table and column comments</label>
        <label><input type="checkbox" checked={policy.include_history} onChange={(e) => setPolicy({ ...policy, include_history: e.target.checked })} /> use my query history to find tables</label>
        <label className="col">Never send (one per line: NAME, TABLE.COLUMN, PAY*)
          <textarea aria-label="Never send" rows={3} value={policy.exclude.join("\n")} onChange={(e) => setPolicy({ ...policy, exclude: e.target.value.split("\n").map((x) => x.trim()).filter(Boolean) })} />
        </label>
        {preview ? <div className="muted">{preview.objects.length} objects, ~{preview.tokens_estimate} tokens{preview.excluded ? `, ${preview.excluded} hidden` : ""}: {preview.objects.map((o) => o.id.replace(/^\w+:/, "")).join(", ")}</div> : <div className="muted">Type a question to preview.</div>}
        {preview && <pre className="ctx-text">{preview.text}</pre>}
      </details>
      <div className="ask-input">
        <textarea aria-label="Question" rows={3} value={text} placeholder={mode === "fix" ? "What went wrong?" : "Ask about this schema…"} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void send(); }} />
        {busy ? <button onClick={() => cancel.current?.()}>Stop</button> : <button className="primary" disabled={!text.trim()} onClick={() => void send()}>Send</button>}
      </div>
    </aside>
  );
}
