import { useEffect, useRef } from "react";
import { EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { PLSQL, sql } from "@codemirror/lang-sql";
import { autocompletion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import type { Completions, Suggestion } from "../types";

interface Props {
  value: string;
  onChange(v: string): void;
  /** Called with the selection if any, otherwise the whole text. */
  onRun(sqlText: string): void;
  complete(text: string, cursor: number): Promise<Completions>;
}

const TYPE: Record<Suggestion["kind"], string> = {
  table: "class", view: "interface", column: "property", schema: "namespace", alias: "variable", keyword: "keyword", join: "function",
};

export function SqlEditor({ value, onChange, onRun, complete }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>();
  // Keep callbacks fresh without recreating the editor.
  const cb = useRef({ onChange, onRun, complete });
  cb.current = { onChange, onRun, complete };

  useEffect(() => {
    const run = (v: EditorView) => {
      const sel = v.state.selection.main;
      const text = sel.empty ? v.state.doc.toString() : v.state.sliceDoc(sel.from, sel.to);
      cb.current.onRun(text);
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
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          history(),
          sql({ dialect: PLSQL, upperCaseKeywords: true }),
          autocompletion({ override: [source], activateOnTyping: true }),
          Prec.highest(keymap.of([{ key: "Mod-Enter", run }, { key: "F5", run }])),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          EditorView.updateListener.of((u) => u.docChanged && cb.current.onChange(u.state.doc.toString())),
        ],
      }),
    });
    view.current = v;
    return () => v.destroy();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== value) v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value } });
  }, [value]);

  return <div className="editor" ref={host} />;
}
