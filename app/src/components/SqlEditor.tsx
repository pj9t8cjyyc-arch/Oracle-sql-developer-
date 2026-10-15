import { useEffect, useRef } from "react";
import { EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { PLSQL, sql } from "@codemirror/lang-sql";

interface Props {
  value: string;
  onChange(v: string): void;
  /** Called with the selection if any, otherwise the whole text. */
  onRun(sqlText: string): void;
}

export function SqlEditor({ value, onChange, onRun }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>();
  // Keep callbacks fresh without recreating the editor.
  const cb = useRef({ onChange, onRun });
  cb.current = { onChange, onRun };

  useEffect(() => {
    const run = (v: EditorView) => {
      const sel = v.state.selection.main;
      const text = sel.empty ? v.state.doc.toString() : v.state.sliceDoc(sel.from, sel.to);
      cb.current.onRun(text);
      return true;
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
