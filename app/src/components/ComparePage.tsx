import { useState } from "react";
import type { Api } from "../api";
import { CompareData } from "./CompareData";
import { CompareSchema } from "./CompareSchema";
import { CompareText, type ObjRef, type SheetRef } from "./CompareText";
import type { ConnState } from "./Explorer";

type Mode = "text" | "data" | "schema";

/** Three kinds of comparison in one place. All three stay mounted so switching keeps your work. */
export function ComparePage({ api, conns, sheets, onWorksheet, onError }: {
  api: Api; conns: ConnState[]; sheets: SheetRef[]; onWorksheet(connId: string, owner: string, sql: string): void; onError(e: unknown): void;
}) {
  const [mode, setMode] = useState<Mode>("text");
  const [preset, setPreset] = useState<{ a: ObjRef; b: ObjRef; nonce: number }>();
  return (
    <div className="cmp-page">
      <div className="cmp-modes">
        <div className="segmented" role="tablist" aria-label="Kind of comparison">
          {([["text", "Text / code"], ["data", "Data by key"], ["schema", "Schema"]] as [Mode, string][]).map(([m, l]) => <button key={m} role="tab" aria-selected={mode === m} className={mode === m ? "on" : ""} onClick={() => setMode(m)}>{l}</button>)}
        </div>
        <span className="muted small">{mode === "text" ? "SQL, PL/SQL, DDL or any text file, line by line with word highlights." : mode === "data" ? "Two files, queries or tables: rows matched by key, compared column by column." : "Two schemas, usually on two databases: what differs, and a draft script."}</span>
      </div>
      <div hidden={mode !== "text"}><CompareText api={api} conns={conns} sheets={sheets} preset={preset} onError={onError} /></div>
      <div hidden={mode !== "data"}><CompareData api={api} conns={conns} onError={onError} /></div>
      <div hidden={mode !== "schema"}><CompareSchema api={api} conns={conns} onError={onError} onWorksheet={onWorksheet} onTextDiff={(a, b) => { setPreset({ a, b, nonce: Date.now() }); setMode("text"); }} /></div>
    </div>
  );
}
