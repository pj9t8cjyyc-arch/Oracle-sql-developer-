// Script splitting and statement classification for the browser demo. It follows
// crates/ora-core/src/{lexer,sqltext,guard}.rs rule for rule; the shared cases in
// crates/ora-core/tests/fixtures/split_cases.json are checked against both (npm run test:split).

type Tok = { t: "word"; v: string } | { t: "quoted"; v: string } | { t: "punct"; v: string } | { t: "other" };
interface Token { tok: Tok; start: number; end: number }

export function lex(src: string): Token[] {
  const out: Token[] = [];
  const n = src.length;
  let i = 0;
  const isAlpha = (c: string) => /\p{L}/u.test(c);
  const isAlnum = (c: string) => /[\p{L}\p{N}]/u.test(c);
  while (i < n) {
    const ch = src[i];
    const s = i;
    if (/\s/.test(ch)) i++;
    else if (ch === "-" && src[i + 1] === "-") { while (i < n && src[i] !== "\n") i++; }
    else if (ch === "/" && src[i + 1] === "*") { i += 2; while (i + 1 < n && !(src[i] === "*" && src[i + 1] === "/")) i++; i = Math.min(i + 2, n); }
    else if ((ch === "q" || ch === "Q") && i + 2 < n && src[i + 1] === "'" && !/\s/.test(src[i + 2])) {
      const open = src[i + 2];
      const close = ({ "[": "]", "{": "}", "(": ")", "<": ">" } as Record<string, string>)[open] ?? open;
      i += 3;
      while (i + 1 < n && !(src[i] === close && src[i + 1] === "'")) i++;
      i = Math.min(i + 2, n);
      out.push({ tok: { t: "other" }, start: s, end: i });
    } else if (ch === "'") {
      i++;
      while (i < n) { if (src[i] === "'") { if (src[i + 1] === "'") { i += 2; continue; } break; } i++; }
      i = Math.min(i + 1, n);
      out.push({ tok: { t: "other" }, start: s, end: i });
    } else if (ch === '"') {
      i++;
      const b = i;
      while (i < n && src[i] !== '"') i++;
      const name = src.slice(b, i);
      i = Math.min(i + 1, n);
      out.push({ tok: { t: "quoted", v: name }, start: s, end: i });
    } else if (isAlpha(ch) || ch === "_") {
      while (i < n && (isAlnum(src[i]) || "_$#".includes(src[i]))) i++;
      out.push({ tok: { t: "word", v: src.slice(s, i).toUpperCase() }, start: s, end: i });
    } else if (/[0-9]/.test(ch)) {
      while (i < n && (/[A-Za-z0-9]/.test(src[i]) || src[i] === ".")) i++;
      out.push({ tok: { t: "other" }, start: s, end: i });
    } else { i++; out.push({ tok: { t: "punct", v: ch }, start: s, end: i }); }
  }
  return out;
}

const words = (toks: Token[]) => toks.filter((t) => t.tok.t === "word").map((t) => (t.tok as { v: string }).v);
const first = (toks: Token[]) => words(toks)[0];
const STORED = ["FUNCTION", "PROCEDURE", "PACKAGE", "TRIGGER", "TYPE"];

function isPlsql(toks: Token[]): boolean {
  const f = first(toks);
  if (["BEGIN", "DECLARE", "CALL", "EXEC", "EXECUTE"].includes(f)) return true;
  if (f === "CREATE") {
    const w = words(toks).slice(1).find((x) => !["OR", "REPLACE", "EDITIONABLE", "NONEDITIONABLE"].includes(x));
    return w !== undefined && STORED.includes(w);
  }
  return false;
}
const isExec = (toks: Token[]) => ["EXEC", "EXECUTE"].includes(first(toks));
const isBlock = (toks: Token[]) => isPlsql(toks) && !isExec(toks) && first(toks) !== "CALL";

export type Kind = "select" | "insert" | "update" | "delete" | "merge" | "ddl" | "plsql" | "transaction" | "other" | "empty";

export function kindOf(text: string): Kind {
  const toks = lex(text);
  const f = first(toks);
  if (f === undefined) return "empty";
  if (isPlsql(toks)) return "plsql";
  if (f === "SELECT" || f === "WITH") return "select";
  if (["INSERT", "UPDATE", "DELETE", "MERGE"].includes(f)) return f.toLowerCase() as Kind;
  if (["CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME", "COMMENT", "GRANT", "REVOKE", "FLASHBACK", "PURGE"].includes(f)) return "ddl";
  if (["COMMIT", "ROLLBACK", "SAVEPOINT", "SET"].includes(f)) return "transaction";
  return "other";
}

/** What is actually sent to Oracle for one statement. */
export function sentSql(text: string): string {
  const toks = lex(text);
  if (isBlock(toks)) return text.trim();
  if (isExec(toks)) {
    const k = toks.find((t) => t.tok.t === "word" && ["EXEC", "EXECUTE"].includes((t.tok as { v: string }).v))!;
    const body = text.slice(k.end).trim().replace(/;+$/, "").trimEnd();
    return `BEGIN ${body}; END;`;
  }
  const t = text.trim();
  return (t.endsWith(";") ? t.slice(0, -1) : t).trim();
}

export interface Stmt { start: number; end: number; text: string; kind: Kind }

function slashAlone(src: string, t: Token): boolean {
  if (t.tok.t !== "punct" || t.tok.v !== "/") return false;
  const ls = src.lastIndexOf("\n", t.start - 1) + 1;
  let le = src.indexOf("\n", t.end);
  if (le < 0) le = src.length;
  return src.slice(ls, t.start).trim() === "" && src.slice(t.end, le).trim() === "";
}

export function splitStatements(src: string): Stmt[] {
  const toks = lex(src);
  const out: Stmt[] = [];
  let segStart = 0;
  let firstTok: number | null = null;
  let depth = 0;
  let plsql = false;
  let skipUntil = 0;
  const flush = (from: number, to: number) => {
    const slice = src.slice(from, Math.min(to, src.length));
    const trimmed = slice.trim();
    if (!trimmed) return;
    const lead = slice.length - slice.trimStart().length;
    const kind = kindOf(trimmed);
    if (kind === "empty") return;
    out.push({ start: from + lead, end: from + lead + trimmed.length, text: trimmed, kind });
  };
  toks.forEach((t, i) => {
    if (t.start < skipUntil) return;
    if (firstTok === null) {
      firstTok = i;
      const rest = toks.slice(i, i + 8);
      plsql = isBlock(rest);
      depth = 0;
      if (isExec(rest)) {
        let eol = src.indexOf("\n", t.start);
        if (eol < 0) eol = src.length;
        flush(segStart, eol);
        segStart = eol; skipUntil = eol; firstTok = null;
        return;
      }
    }
    if (t.tok.t === "punct") {
      if (t.tok.v === "(") depth++;
      else if (t.tok.v === ")") depth--;
      else if (t.tok.v === ";" && !plsql && depth <= 0) { flush(segStart, t.start); segStart = t.end; firstTok = null; }
      else if (t.tok.v === "/" && plsql && slashAlone(src, t)) { flush(segStart, t.start); segStart = t.end; firstTok = null; }
    }
  });
  flush(segStart, src.length);
  return out;
}
