// Demo-only stand-in for the Rust package analysis and graph (ora-core `packages` / `graph`),
// so the UI can be built and checked in a browser. The real logic and its tests live in ora-core.
import type { Api } from "./api";
import type { DossierOptions, GEdge, GNode, GraphFocus, KbSummary, MemberRef, PackageAnalysis, PackageDetail, PkgMember, SNode, Subgraph, TableUse } from "./types";

type M = { n: string; sig: string; fn?: boolean; pub?: boolean; reads?: string[]; writes?: string[]; calls?: string[]; internal?: number };
const SPEC: Record<string, { wrapped?: boolean; invalid?: boolean; lines: number; members: M[] }> = {
  EMP_PKG: { lines: 140, members: [
    { n: "RAISE_SALARY", sig: "RAISE_SALARY(P_ID IN NUMBER, P_PCT IN NUMBER)", writes: ["EMPLOYEES"], calls: ["AUDIT_PKG.LOG_CHANGE", "EMP_PKG.CHECK_LIMIT"] },
    { n: "HEADCOUNT", sig: "HEADCOUNT(P_DEPT IN NUMBER) RETURN NUMBER", fn: true, reads: ["EMPLOYEES", "DEPARTMENTS"] },
    { n: "TRANSFER", sig: "TRANSFER(P_ID IN NUMBER, P_DEPT IN NUMBER)", writes: ["EMPLOYEES", "JOB_HISTORY"], calls: ["AUDIT_PKG.LOG_CHANGE"] },
    { n: "CHECK_LIMIT", sig: "CHECK_LIMIT(P_PCT NUMBER)", pub: false, internal: 1 },
    { n: "OLD_BONUS", sig: "OLD_BONUS(P_ID NUMBER)", pub: false, writes: ["EMPLOYEES"] },
  ] },
  PAYROLL_PKG: { lines: 520, members: [
    { n: "RUN_MONTHLY", sig: "RUN_MONTHLY(P_MONTH IN DATE)", reads: ["EMPLOYEES", "JOB_HISTORY"], writes: ["PAYSLIPS"], calls: ["EMP_PKG.HEADCOUNT", "AUDIT_PKG.LOG_CHANGE", "UTIL_PKG.SEND_MAIL"] },
    { n: "NET_PAY", sig: "NET_PAY(P_ID IN NUMBER, P_GROSS IN NUMBER) RETURN NUMBER", fn: true, reads: ["PAYSLIPS"] },
    { n: "REVERSE_RUN", sig: "REVERSE_RUN(P_MONTH IN DATE)", writes: ["PAYSLIPS"] },
  ] },
  REPORT_PKG: { lines: 260, members: [
    { n: "DEPT_REPORT", sig: "DEPT_REPORT(P_DEPT IN NUMBER) RETURN SYS_REFCURSOR", fn: true, reads: ["DEPARTMENTS", "EMP_DETAILS_VIEW"], calls: ["EMP_PKG.HEADCOUNT"] },
    { n: "PAY_SUMMARY", sig: "PAY_SUMMARY(P_MONTH IN DATE) RETURN SYS_REFCURSOR", fn: true, reads: ["PAYSLIPS"], calls: ["PAYROLL_PKG.NET_PAY"] },
  ] },
  UTIL_PKG: { lines: 90, members: [
    { n: "SEND_MAIL", sig: "SEND_MAIL(P_TO IN VARCHAR2, P_BODY IN VARCHAR2)" },
    { n: "TO_ISO", sig: "TO_ISO(P_D IN DATE) RETURN VARCHAR2", fn: true },
  ] },
  AUDIT_PKG: { lines: 70, invalid: true, members: [
    { n: "LOG_CHANGE", sig: "LOG_CHANGE(P_TABLE IN VARCHAR2, P_ID IN NUMBER)", writes: ["EMP_AUDIT"], calls: ["UTIL_PKG.TO_ISO"] },
  ] },
  LEGACY_PKG: { wrapped: true, lines: 0, members: [{ n: "MIGRATE", sig: "MIGRATE(P_BATCH IN NUMBER)" }] },
};

function build(): PackageAnalysis {
  const pk = (c: string) => c.split(".")[0], mm = (c: string) => c.split(".")[1];
  const callers = new Map<string, MemberRef[]>();
  for (const [p, s] of Object.entries(SPEC)) for (const m of s.members) for (const c of m.calls ?? []) {
    if (pk(c) === p) continue;
    const k = c; callers.set(k, [...(callers.get(k) ?? []), { package: p, member: m.n }]);
  }
  const packages: PackageDetail[] = Object.entries(SPEC).map(([name, s]) => {
    const internal = new Map<string, number>();
    for (const m of s.members) for (const c of m.calls ?? []) if (pk(c) === name) internal.set(mm(c), (internal.get(mm(c)) ?? 0) + 1);
    let unref = 0, unusedPriv = 0;
    const members: PkgMember[] = s.members.map((m) => {
      const called_by = callers.get(`${name}.${m.n}`) ?? [];
      const internal_refs = internal.get(m.n) ?? 0;
      const pub = m.pub !== false;
      const scanned = !s.wrapped;
      const status = scanned && called_by.length === 0 && internal_refs === 0 ? (pub ? "unreferenced" : "unused_private") : "used";
      if (status === "unreferenced") unref++; if (status === "unused_private") unusedPriv++;
      return { name: m.n, kind: m.fn ? "FUNCTION" : "PROCEDURE", signature: m.sig, overload: null, public: pub, source: name === "EMP_PKG" || name === "PAYROLL_PKG" ? "plscope" : "scan", scan_only: name === "EMP_PKG" && m.n === "OLD_BONUS" ? ["writes JOB_HISTORY"] : [],
        calls: (m.calls ?? []).filter((c) => pk(c) !== name).map((c) => ({ package: pk(c), member: mm(c) })),
        reads: m.reads ?? [], writes: m.writes ?? [], called_by, internal_refs, status };
    });
    const tabs = new Map<string, TableUse>();
    for (const m of members) { for (const t of m.reads) tabs.set(t, { table: t, read: true, write: tabs.get(t)?.write ?? false }); for (const t of m.writes) tabs.set(t, { table: t, read: tabs.get(t)?.read ?? false, write: true }); }
    const calls_packages = [...new Set(members.flatMap((m) => m.calls.map((c) => c.package)))].sort();
    return { name, spec_status: "VALID", body_status: s.invalid ? "INVALID" : "VALID", wrapped: !!s.wrapped, body_lines: s.lines, members, calls_packages, called_by: [], tables: [...tabs.values()].sort((a, b) => a.table.localeCompare(b.table)), unreferenced: unref, unused_private: unusedPriv, analysis: s.wrapped ? "none" : name === "EMP_PKG" || name === "PAYROLL_PKG" ? "plscope" : "scan", unattributed: 0 };
  });
  for (const p of packages) for (const c of p.calls_packages) packages.find((x) => x.name === c)?.called_by.push(p.name);
  return { owner: "HR", packages, wrapped: packages.filter((p) => p.wrapped).length, invalid: packages.filter((p) => p.body_status !== "VALID").length, plscope_packages: packages.filter((p) => p.analysis === "plscope").length, scope_error: null };
}

const id = (kind: string, name: string) => `${kind}:HR.${name}`;
function buildGraph() {
  const nodes = new Map<string, GNode>();
  const add = (kind: string, name: string) => { const i = id(kind, name); if (!nodes.has(i)) nodes.set(i, { id: i, owner: "HR", name, kind }); return i; };
  const edges: GEdge[] = [];
  const edge = (a: string, b: string, kind: GEdge["kind"]) => { if (a !== b && !edges.some((e) => e.from === a && e.to === b && e.kind === kind)) edges.push({ from: a, to: b, kind }); };
  for (const t of ["EMPLOYEES", "DEPARTMENTS", "JOB_HISTORY", "LOCATIONS", "PAYSLIPS", "EMP_AUDIT"]) add("TABLE", t);
  edge(id("TABLE", "EMPLOYEES"), id("TABLE", "DEPARTMENTS"), "fk"); edge(id("TABLE", "JOB_HISTORY"), id("TABLE", "EMPLOYEES"), "fk");
  edge(id("TABLE", "DEPARTMENTS"), id("TABLE", "LOCATIONS"), "fk"); edge(id("TABLE", "PAYSLIPS"), id("TABLE", "EMPLOYEES"), "fk");
  const v = add("VIEW", "EMP_DETAILS_VIEW"); edge(v, id("TABLE", "EMPLOYEES"), "uses"); edge(v, id("TABLE", "DEPARTMENTS"), "uses");
  const trg: [string, string, string?][] = [["EMP_BI_TRG", "EMPLOYEES", "EMPLOYEES_SEQ"], ["EMP_AUDIT_TRG", "EMPLOYEES", "AUDIT_SEQ"], ["DEPT_BI_TRG", "DEPARTMENTS", "DEPARTMENTS_SEQ"]];
  for (const [t, tab, sq] of trg) { const n = add("TRIGGER", t); edge(n, id("TABLE", tab), "uses"); if (sq) edge(n, add("SEQUENCE", sq), "uses"); }
  edge(add("PROCEDURE", "ADD_JOB_HISTORY"), id("TABLE", "JOB_HISTORY"), "uses");
  edge(add("FUNCTION", "GET_SALARY"), id("TABLE", "EMPLOYEES"), "uses");
  const a = build();
  for (const p of a.packages) {
    const n = add("PACKAGE", p.name);
    for (const t of p.tables) edge(n, nodes.has(id("TABLE", t.table)) ? id("TABLE", t.table) : id("VIEW", t.table), "uses");
    for (const c of p.calls_packages) edge(n, add("PACKAGE", c), "uses");
  }
  return { nodes, edges };
}

export function graphApi(): Pick<Api, "packageAnalysis" | "graphInfo" | "graphSearch" | "graphFocus" | "kbBuild" | "kbStatus" | "kbPreview" | "kbExport"> {
  const g = buildGraph();
  const analysis = build();
  let built: KbSummary | null = null;
  const summary = (owner: string): KbSummary => ({
    owner, taken_at: Date.now(), tables: 6, views: 1, packages: analysis.packages.length, members: analysis.packages.reduce((n, p) => n + p.members.length, 0), triggers: 3, indexes: 6, sequences: 3,
    nodes: g.nodes.size, edges: g.edges.length, exact_links: g.edges.length, heuristic_links: 14,
    coverage: { packages: analysis.packages.length, packages_scanned: analysis.packages.length - 1, packages_wrapped: 1, packages_plscope: 2, members: 14, dynamic_sql_sites: 0, gaps: [
      { severity: "warning", area: "packages", message: "1 wrapped package(s) cannot be read, so what they call, read or write is unknown: LEGACY_PKG." },
      { severity: "warning", area: "invalid objects", message: "2 object(s) are INVALID, so their dependencies may be stale: view EMP_DETAILS_VIEW, package AUDIT_PKG." },
      { severity: "info", area: "package calls", message: "Reads, writes and calls inside packages come from a text scan (heuristic). PL/Scope is not used yet." },
      { severity: "info", area: "callers", message: "0 object(s) in other schemas use this schema (shown). Applications and external callers are not visible to the database." },
    ] },
  });
  const dossier = (o: DossierOptions): { md: string; files: string[] } => {
    const n = g.nodes.get(o.focus);
    if (!n) throw "that object is not in the knowledge base";
    const rel = g.edges.filter((e) => e.from === o.focus || e.to === o.focus);
    const line = (e: GEdge) => { const other = g.nodes.get(e.from === o.focus ? e.to : e.from)!; return `- ${other.owner}.${other.name} (${other.kind.toLowerCase()}, ${e.kind}) [${e.kind === "reads" || e.kind === "writes" || e.kind === "calls" ? "scan, heuristic" : "dictionary, exact"}]`; };
    const pk = analysis.packages.find((p) => n.kind === "PACKAGE" && p.name === n.name);
    const members = pk ? `\n## Members\n\n| Member | Scope | Reads | Writes | Calls | Status |\n|---|---|---|---|---|---|\n${pk.members.map((m) => `| \`${m.signature}\` | ${m.public ? "public" : "private"} | ${m.reads.join(", ")} | ${m.writes.join(", ")} | ${m.calls.map((c) => `${c.package}.${c.member}`).join(", ")} | ${m.status} |`).join("\n")}\n` : "";
    const md = `<!-- file: README.md -->\n---\ntype: ora-ide-knowledge-export\nfocus: "${o.focus}"\nowner: ${n.owner}\ndirection: ${o.direction}\nhops: ${o.hops}\n---\n\n# Knowledge export: ${n.owner}.${n.name} (${n.kind.toLowerCase()})\n\n> Demo data. The real export also contains columns, keys, indexes, trigger source, DDL and the package code that touches the object.\n\n## What this export cannot tell you\n\n${summary(n.owner).coverage.gaps.map((x) => `- **${x.area}** (${x.severity}): ${x.message}`).join("\n")}\n\n<!-- file: objects/${n.kind.toLowerCase()}/${n.owner}.${n.name}.md -->\n---\nid: "${n.id}"\nkind: ${n.kind.toLowerCase()}\n---\n\n# ${n.owner}.${n.name} (${n.kind.toLowerCase()})\n\n## Links\n\n${rel.map(line).join("\n") || "- none"}\n${members}`;
    return { md, files: ["README.md", `objects/${n.kind.toLowerCase()}/${n.owner}.${n.name}.md`, "graph.json", "manifest.json"] };
  };
  const empty = (owner: string): PackageAnalysis => ({ owner, packages: [], wrapped: 0, invalid: 0, plscope_packages: 0, scope_error: null });
  return {
    kbBuild: async (_c, owner) => { await new Promise((r) => setTimeout(r, 400)); built = summary(owner); return built; },
    kbStatus: async () => built,
    kbPreview: async (_c, _o, o) => { const d = dossier(o); return { markdown: d.md, files: d.files, tokens_estimate: Math.round(d.md.length / 4), truncated: false, missing: o.include_source ? ["demo: source text is not available in demo mode"] : [], preview_cut: false }; },
    kbExport: async (_c, _o, o, path, zip) => {
      const d = dossier(o);
      const blob = new Blob([d.md], { type: "text/markdown" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob); a.download = path.split("/").pop()!.replace(/\.zip$/, ".md"); a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 1000);
      return { path, bytes: blob.size, files: zip ? d.files.length : 1, tokens_estimate: Math.round(d.md.length / 4) };
    },
    packageAnalysis: async (_c, owner) => { await new Promise((r) => setTimeout(r, 120)); return owner === "HR" ? analysis : empty(owner); },
    graphInfo: async (_c, owner) => {
      const kinds: Record<string, number> = {};
      if (owner === "HR") for (const n of g.nodes.values()) kinds[n.kind] = (kinds[n.kind] ?? 0) + 1;
      return { owner, nodes: owner === "HR" ? g.nodes.size : 0, edges: owner === "HR" ? g.edges.length : 0, kinds };
    },
    graphSearch: async (_c, owner, q) => {
      if (owner !== "HR") return [];
      const u = q.trim().toUpperCase();
      const all = [...g.nodes.values()].filter((n) => !u || n.name.includes(u));
      const pre = all.filter((n) => n.name.startsWith(u)).sort((a, b) => a.name.localeCompare(b.name));
      return [...pre, ...all.filter((n) => !n.name.startsWith(u)).sort((a, b) => a.name.localeCompare(b.name))].slice(0, 30);
    },
    graphFocus: async (_c, _o, f: GraphFocus): Promise<Subgraph> => {
      if (!g.nodes.has(f.id)) throw "that object is not in the graph";
      const hops = Math.min(8, Math.max(1, f.hops)), max = f.max_nodes ?? 150;
      const ok = (i: string) => !f.kinds || f.kinds.includes(g.nodes.get(i)!.kind);
      const depth = new Map<string, number>([[f.id, 0]]);
      let truncated = false;
      for (const sign of [1, -1]) {
        if ((sign === 1 && f.direction === "used_by") || (sign === -1 && f.direction === "uses")) continue;
        const seen = new Set([f.id]); let q: [string, number][] = [[f.id, 0]];
        while (q.length) {
          const [cur, d] = q.shift()!;
          if (d >= hops) continue;
          for (const e of g.edges) {
            const [from, next] = sign === 1 ? [e.from, e.to] : [e.to, e.from];
            if (from !== cur || seen.has(next) || !ok(next)) continue;
            if (depth.size >= max) { truncated = true; continue; }
            seen.add(next); if (!depth.has(next)) depth.set(next, sign * (d + 1)); q.push([next, d + 1]);
          }
        }
      }
      const shown = new Set(depth.keys());
      const more = (i: string, d: number) => {
        if (Math.abs(d) !== hops && !(truncated && d !== 0)) return 0;
        const x = new Set<string>();
        for (const e of g.edges) {
          if (d >= 0 && f.direction !== "used_by" && e.from === i && !shown.has(e.to) && ok(e.to)) x.add(e.to);
          if (d <= 0 && f.direction !== "uses" && e.to === i && !shown.has(e.from) && ok(e.from)) x.add(e.from);
        }
        return x.size;
      };
      const nodes: SNode[] = [...depth].map(([i, d]) => ({ ...g.nodes.get(i)!, depth: d, more: more(i, d) }))
        .sort((a, b) => a.depth - b.depth || a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name));
      return { focus: f.id, nodes, edges: g.edges.filter((e) => shown.has(e.from) && shown.has(e.to)), truncated };
    },
  };
}
