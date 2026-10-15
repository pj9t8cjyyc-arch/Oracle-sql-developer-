import { useEffect, useRef, useState } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { PLSQL, sql } from "@codemirror/lang-sql";
import { Icon } from "../ui-icons";

/** Read-only SQL/PL-SQL viewer with a copy button. */
export function CodeView({ value, empty }: { value: string; empty?: string }) {
  const host = useRef<HTMLDivElement>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: value,
        extensions: [lineNumbers(), sql({ dialect: PLSQL }), EditorState.readOnly.of(true), EditorView.editable.of(false), EditorView.lineWrapping],
      }),
    });
    return () => v.destroy();
  }, [value]);

  if (!value.trim()) return <div className="empty-note">{empty ?? "Nothing to show."}</div>;
  return (
    <div className="codeview">
      <button
        className="ghost copy" title="Copy"
        onClick={() => { void navigator.clipboard?.writeText(value).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1200); }).catch(() => {}); }}
      >
        <Icon name={copied ? "check" : "copy"} size={14} /> {copied ? "Copied" : "Copy"}
      </button>
      <div ref={host} />
    </div>
  );
}
