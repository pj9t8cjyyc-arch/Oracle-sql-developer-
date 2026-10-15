import { useEffect, useRef } from "react";
import { EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { PLSQL, sql } from "@codemirror/lang-sql";
import { autocompletion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import type { Completions, HistoryHit, Suggestion } from "../types";
import { timeAgo } from "../format";

export interface RunRequest {
  /** Selected text, if any: it always wins. */
  selection: string | null;
  doc: string;
  /** UTF-16 offset of the cursor. */
  cursor: number;
  /** F5 / "Run script": every statement in order, instead of the one at the cursor. */
  all: boolean;
}

export interface EditorHandle {
  run(all: boolean): void;
  /** The selection, document and cursor as a run would see them. */
  request(): RunRequest;
  /** Replaces the selection, or appends after the last statement when nothing is selected. */
  insert(text: string): void;
}

interface Props {
  value: string;
  onChange(v: string): void;
  onRun(r: RunRequest): void;
  onReady?(h: EditorHandle): void;
  complete(text: string, cursor: number): Promise<Completions>;
  /** Earlier successful statements that start with what has been typed. */
  suggestHistory?(typed: string): Promise<HistoryHit[]>;
}

const TYPE: Record<Suggestion["kind"], string> = {
  table: "class", view: "interface", column: "property", schema: "namespace", alias: "variable", keyword: "keyword", join: "function",
};

export function SqlEditor({ value, onChange, onRun, onReady, complete, suggestHistory }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>();
  const fireRef = useRef<(all: boolean) => void>();
  // Keep callbacks fresh without recreating the editor.
  const cb = useRef({ onChange, onRun, onReady, complete, suggestHistory });
  cb.current = { onChange, onRun, onReady, complete, suggestHistory };

  useEffect(() => {
    const fire = (v: EditorView, all: boolean) => {
      const sel = v.state.selection.main;
      cb.current.onRun({ selection: sel.empty ? null : v.state.sliceDoc(sel.from, sel.to), doc: v.state.doc.toString(), cursor: sel.head, all });
      return true;
    };
    const source = async (ctx: CompletionContext): Promise<CompletionResult | null> => {
      // Auto-open while typing a word or after a dot; Ctrl+Space opens it anywhere.
      if (!ctx.explicit && !ctx.matchBefore(/[\w$#]+/) && !ctx.matchBefore(/\.\w*/)) return null;
      try {
        const r = await cb.current.complete(ctx.state.doc.toString(), ctx.pos);
        if (ctx.aborted || r.items.length === 0) return null;
        return {
          from: r.from,
          filter: false, // the backend already filtered and ranked
          options: r.items.map((i) => ({ label: i.label, apply: i.insert, detail: i.detail ?? undefined, type: TYPE[i.kind], boost: i.score })),
        };
      } catch {
        return null; // completion must never break typing
      }
    };
    // "You ran this before": offer earlier statements that begin with what is typed.
    const historySource = async (ctx: CompletionContext): Promise<CompletionResult | null> => {
      const h = cb.current.suggestHistory;
      if (!h) return null;
      const before = ctx.state.sliceDoc(0, ctx.pos);
      const startRaw = before.lastIndexOf(";") + 1;
      const stmt = before.slice(startRaw);
      const typed = stmt.trim();
      if (typed.length < 3) return null;
      const norm = (x: string) => x.replace(/\s+/g, " ").trim().toLowerCase();
      try {
        const hits = (await h(typed)).filter((x) => norm(x.sql).startsWith(norm(typed)) && norm(x.sql) !== norm(typed));
        if (ctx.aborted || hits.length === 0) return null;
        return {
          from: startRaw + (stmt.length - stmt.trimStart().length),
          filter: false,
          options: hits.map((x) => ({
            label: x.sql.replace(/\s+/g, " ").slice(0, 90), apply: x.sql, type: "text", boost: -50,
            detail: `ran ${x.runs}× · ${timeAgo(x.last_run)}`, info: x.sql,
          })),
        };
      } catch {
        return null;
      }
    };
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          history(),
          sql({ dialect: PLSQL, upperCaseKeywords: true }),
          autocompletion({ override: [source, historySource], activateOnTyping: true }),
          Prec.highest(keymap.of([{ key: "Mod-Enter", run: (v) => fire(v, false) }, { key: "F5", run: (v) => fire(v, true) }])),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          EditorView.updateListener.of((u) => u.docChanged && cb.current.onChange(u.state.doc.toString())),
        ],
      }),
    });
    view.current = v;
    fireRef.current = (all) => { fire(v, all); };
    return () => v.destroy();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The editor instance can outlive a change of parent (e.g. a worksheet binding to its
  // connection), so hand the run handle over on every render, not once at creation.
  useEffect(() => { onReady?.({
    run: (all) => fireRef.current?.(all),
    request: () => {
      const v = view.current!;
      const sel = v.state.selection.main;
      return { selection: sel.empty ? null : v.state.sliceDoc(sel.from, sel.to), doc: v.state.doc.toString(), cursor: sel.head, all: false };
    },
    insert: (text) => {
      const v = view.current;
      if (!v) return;
      const sel = v.state.selection.main;
      if (!sel.empty) { v.dispatch({ changes: { from: sel.from, to: sel.to, insert: text } }); return; }
      const end = v.state.doc.length;
      const sep = end === 0 ? "" : v.state.doc.toString().endsWith("\n") ? "\n" : "\n\n";
      v.dispatch({ changes: { from: end, insert: sep + text } });
    },
  }); });

  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== value) v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value } });
  }, [value]);

  return <div className="editor" ref={host} />;
}
