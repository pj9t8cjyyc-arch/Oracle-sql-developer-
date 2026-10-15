import { useEffect, useState } from "react";
import type { Api } from "../api";
import type { DbObject, TableColumn } from "../types";

interface Props {
  api: Api;
  onInsert(text: string): void;
  onError(e: unknown): void;
}

export function SchemaTree({ api, onInsert, onError }: Props) {
  const [schemas, setSchemas] = useState<string[]>([]);
  const [owner, setOwner] = useState("");
  const [objects, setObjects] = useState<DbObject[]>([]);
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<Record<string, TableColumn[] | null>>({});

  useEffect(() => {
    api.listSchemas().then((s) => { setSchemas(s); if (s[0]) setOwner(s[0]); }).catch(onError);
  }, [api, onError]);

  useEffect(() => {
    if (!owner) return;
    setOpen({});
    api.listObjects(owner).then(setObjects).catch(onError);
  }, [api, owner, onError]);

  const toggle = (o: DbObject) => {
    if (o.name in open) {
      const { [o.name]: _gone, ...rest } = open;
      setOpen(rest);
    } else if (o.object_type === "TABLE" || o.object_type === "VIEW") {
      setOpen({ ...open, [o.name]: null });
      api.listColumns(owner, o.name).then((c) => setOpen((p) => ({ ...p, [o.name]: c }))).catch(onError);
    }
  };

  const shown = objects.filter((o) => o.name.toLowerCase().includes(filter.toLowerCase()));
  const groups = [...new Set(shown.map((o) => o.object_type))];

  return (
    <aside className="tree">
      <select value={owner} onChange={(e) => setOwner(e.target.value)} aria-label="Schema">
        {schemas.map((s) => <option key={s}>{s}</option>)}
      </select>
      <input placeholder="Filter objects…" value={filter} onChange={(e) => setFilter(e.target.value)} />
      <div className="tree-scroll">
        {groups.map((g) => (
          <div key={g}>
            <div className="tree-group">{g}S ({shown.filter((o) => o.object_type === g).length})</div>
            {shown.filter((o) => o.object_type === g).map((o) => (
              <div key={o.name}>
                <div className={`tree-item ${o.valid ? "" : "invalid"}`} onClick={() => toggle(o)} onDoubleClick={() => onInsert(`SELECT * FROM ${owner}.${o.name}`)} title={o.valid ? "Double-click to query" : "INVALID object"}>
                  {o.name in open ? "▾" : "▸"} {o.name}
                </div>
                {o.name in open && (open[o.name] === null ? <div className="tree-col muted">loading…</div> : open[o.name]!.map((c) => (
                  <div key={c.name} className="tree-col" onDoubleClick={() => onInsert(c.name)} title="Double-click to insert">
                    {c.name} <span className="muted">{c.data_type}{c.nullable ? "" : " NOT NULL"}</span>
                  </div>
                )))}
              </div>
            ))}
          </div>
        ))}
      </div>
    </aside>
  );
}
