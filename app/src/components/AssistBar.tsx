import { useState } from "react";
import type { AssistSuggestion } from "../types";

export type AssistState = "idle" | "thinking" | "no_kb" | "auth" | "error";

const KIND: Record<AssistSuggestion["kind"], string> = { fix: "Fix", optimize: "Faster", next: "Next", explain: "Note" };

/** What the assistant noticed, shown where the person is already looking. Never blocks typing. */
export function AssistBar({ state, message, suggestions, sent, building, onInsert, onDismiss, onBuild }: {
  state: AssistState; message: string; suggestions: AssistSuggestion[]; sent?: [number, number]; building: boolean;
  onInsert(sql: string): void; onDismiss(): void; onBuild(): void;
}) {
  const [open, setOpen] = useState<number | null>(null);
  if (state === "no_kb") {
    return (
      <div className="assist-bar" data-testid="assist-bar">
        <span className="assist-ico">✨</span>
        <span>AI needs to know your schema to help while you type.</span>
        <button className="primary" disabled={building} onClick={onBuild}>{building ? "Reading schema…" : "Read schema now"}</button>
      </div>
    );
  }
  if (state === "auth") return <div className="assist-bar warn" data-testid="assist-bar"><span className="assist-ico">✨</span><span>{message || "AI sign-in needed"} — automatic help is paused.</span></div>;
  if (state === "error") return <div className="assist-bar warn" data-testid="assist-bar"><span className="assist-ico">✨</span><span>Automatic help hit a problem and will retry shortly: {message}</span></div>;
  if (suggestions.length === 0) return null;
  return (
    <div className="assist-bar col" data-testid="assist-bar">
      <div className="assist-row">
        <span className="assist-ico">✨</span>
        {suggestions.map((s, i) => (
          <button key={i} className={`assist-chip k-${s.kind} ${open === i ? "open" : ""}`} onClick={() => setOpen(open === i ? null : i)} title={s.detail}>
            <b>{KIND[s.kind]}</b> {s.title}
          </button>
        ))}
        <span className="grow" />
        {sent && <span className="muted assist-sent" title="Schema objects and approximate tokens sent for this suggestion">sent {sent[0]} objects · ~{sent[1]} tokens</span>}
        <button className="ghost" onClick={onDismiss} aria-label="Dismiss suggestions">✕</button>
      </div>
      {open !== null && suggestions[open] && (
        <div className="assist-detail">
          <div>{suggestions[open].detail}</div>
          {suggestions[open].sql && <>
            <pre>{suggestions[open].sql}</pre>
            <button onClick={() => onInsert(suggestions[open].sql!)}>Insert into worksheet</button>
          </>}
        </div>
      )}
    </div>
  );
}
