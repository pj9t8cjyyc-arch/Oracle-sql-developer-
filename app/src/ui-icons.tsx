import type { ObjectKind } from "./types";

const P: Record<string, string> = {
  database: "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3Z M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6 M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6",
  table: "M3 5h18v14H3z M3 10h18 M9 5v14",
  view: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12Z M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
  mview: "M12 3 3 8l9 5 9-5-9-5Z M3 13l9 5 9-5",
  index: "M4 6h10 M4 12h7 M4 18h4 M17 5v14 m-3-3 3 3 3-3",
  sequence: "M5 9h14 M5 15h14 M10 4 8 20 M16 4l-2 16",
  trigger: "M13 2 4 14h7l-1 8 9-12h-7l1-8Z",
  procedure: "M8 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h2 M16 4h2a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-2 M9 12h6",
  function: "m8 8-4 4 4 4 M16 8l4 4-4 4 M14 5l-4 14",
  package: "M21 8 12 3 3 8v8l9 5 9-5V8Z M3 8l9 5 9-5 M12 13v8",
  type: "M8 4c-2 0-3 1-3 3v2c0 1-1 3-2 3 1 0 2 2 2 3v2c0 2 1 3 3 3 M16 4c2 0 3 1 3 3v2c0 1 1 3 2 3-1 0-2 2-2 3v2c0 2-1 3-3 3",
  synonym: "M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1 M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1",
  sql: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5Z M14 3v5h5 M10 13l-2 2 2 2 M14 13l2 2-2 2",
  search: "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14Z M21 21l-4.3-4.3",
  plus: "M12 5v14 M5 12h14",
  x: "M6 6l12 12 M18 6 6 18",
  right: "m9 6 6 6-6 6",
  down: "m6 9 6 6 6-6",
  stop: "M6 6h12v12H6z",
  refresh: "M20 11a8 8 0 0 0-14.9-3M4 5v4h4 M4 13a8 8 0 0 0 14.9 3M20 19v-4h-4",
  copy: "M9 9h11v11H9z M5 15V5h10",
  key: "M14 8a4 4 0 1 1-3.5 6L4 20v-3h3v-2h2",
  plug: "M9 2v6 M15 2v6 M6 8h12v4a6 6 0 0 1-12 0V8Z M12 18v4",
  columns: "M3 5h18v14H3z M9 5v14 M15 5v14",
  out: "M7 17 17 7 M8 7h9v9",
  alert: "M12 4 2.5 20h19L12 4Z M12 10v4 M12 17h.01",
  check: "m5 12 5 5 9-10",
  grid: "M3 3h8v8H3z M13 3h8v8h-8z M3 13h8v8H3z M13 13h8v8h-8z",
  history: "M3 12a9 9 0 1 0 3-6.7 M3 4v5h5 M12 7v5l3 2",
  star: "m12 3 2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9L12 3Z",
  trash: "M4 7h16 M9 7V4h6v3 M6 7l1 13h10l1-13 M10 11v6 M14 11v6",
  pause: "M8 5v14 M16 5v14",
  download: "M12 4v11 m-4-4 4 4 4-4 M5 20h14",
  upload: "M12 16V5 m-4 4 4-4 4 4 M5 20h14",
};

export type IconName = keyof typeof P;

export function Icon({ name, size = 16, className }: { name: IconName; size?: number; className?: string }) {
  const fill = name === "stop" ? "currentColor" : "none";
  return (
    <svg className={className} width={size} height={size} viewBox="0 0 24 24" fill={fill} stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d={P[name]} />
    </svg>
  );
}

export const KIND_ICON: Record<ObjectKind, IconName> = {
  TABLE: "table", VIEW: "view", "MATERIALIZED VIEW": "mview", INDEX: "index", SEQUENCE: "sequence", TRIGGER: "trigger",
  PROCEDURE: "procedure", FUNCTION: "function", PACKAGE: "package", TYPE: "type", SYNONYM: "synonym",
};

/** Colour role per object kind, so lists are scannable at a glance. */
export const KIND_TONE: Record<ObjectKind, string> = {
  TABLE: "t-table", VIEW: "t-view", "MATERIALIZED VIEW": "t-view", INDEX: "t-idx", SEQUENCE: "t-seq", TRIGGER: "t-trg",
  PROCEDURE: "t-code", FUNCTION: "t-code", PACKAGE: "t-code", TYPE: "t-code", SYNONYM: "t-syn",
};

export function KindIcon({ kind, size = 16 }: { kind: ObjectKind; size?: number }) {
  return <span className={`kind-icon ${KIND_TONE[kind]}`}><Icon name={KIND_ICON[kind]} size={size} /></span>;
}
