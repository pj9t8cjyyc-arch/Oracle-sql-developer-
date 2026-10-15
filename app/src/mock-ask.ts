// Demo-only stand-ins for explain plan and the Ask panel (ora-core `plan` and `context`).
import type { Api } from "./api";
import type { AiEvent, AskContext, Plan, PlanNode, SqlIssue, AssistSuggestion } from "./types";

const TABLES: Record<string, string[]> = {
  EMPLOYEES: ["EMPLOYEE_ID", "FIRST_NAME", "LAST_NAME", "EMAIL", "HIRE_DATE", "JOB_ID", "SALARY", "DEPARTMENT_ID"],
  DEPARTMENTS: ["DEPARTMENT_ID", "DEPARTMENT_NAME", "MANAGER_ID", "LOCATION_ID"],
  LOCATIONS: ["LOCATION_ID", "CITY", "COUNTRY_ID"],
  JOBS: ["JOB_ID", "JOB_TITLE"],
};

function node(p: Partial<PlanNode> & { id: number; operation: string }): PlanNode {
  return { parent_id: null, depth: 0, object_owner: null, object_name: null, object_alias: null, cost: null, self_cost: null, cost_pct: null, rows: null, bytes: null, seconds: null, access: null, filter: null, children: [], hot: false, ...p };
}

function plan(sql: string): Plan {
  if (!/^\s*(select|insert|update|delete|merge|with)\b/i.test(sql)) throw "ORA-00900: invalid SQL statement";
  const full = /employees/i.test(sql);
  const nodes = [
    node({ id: 0, operation: "SELECT STATEMENT", cost: 412, self_cost: 0, cost_pct: 0, rows: 1200, children: [1] }),
    node({ id: 1, parent_id: 0, depth: 1, operation: "HASH JOIN", cost: 412, self_cost: 40, cost_pct: 10, rows: 1200, access: '"E"."DEPARTMENT_ID"="D"."DEPARTMENT_ID"', children: [2, 3] }),
    node({ id: 2, parent_id: 1, depth: 2, operation: "TABLE ACCESS FULL", object_owner: "HR", object_name: "DEPARTMENTS", object_alias: "D@SEL$1", cost: 3, self_cost: 3, cost_pct: 1, rows: 27 }),
    node({ id: 3, parent_id: 1, depth: 2, operation: full ? "TABLE ACCESS FULL" : "INDEX RANGE SCAN", object_owner: "HR", object_name: full ? "EMPLOYEES" : "EMP_DEPT_IX", object_alias: "E@SEL$1", cost: 369, self_cost: 369, cost_pct: 90, rows: 1200, bytes: 84000, filter: 'UPPER("E"."LAST_NAME")=\'KING\'', hot: true }),
  ];
  const findings: Plan["findings"] = full ? [
    { severity: "warn", code: "full_scan", title: "Full scan of HR.EMPLOYEES", detail: "Step 3 reads every row of a table that is expected to return 1,200 rows.", steps: [3], suggestion: "Check the existing indexes on HR.EMPLOYEES first; a candidate is (LAST_NAME)." },
    { severity: "warn", code: "function_on_column", title: "UPPER() on LAST_NAME prevents an index", detail: "A function on a column stops a plain index from being used.", steps: [3], suggestion: "Compare with the column directly, or create a function-based index on UPPER(LAST_NAME)." },
    { severity: "info", code: "hot_step", title: "Step 3 is 90% of the cost", detail: "Tune this step first.", steps: [3], suggestion: null },
  ] : [{ severity: "info", code: "hot_step", title: "Step 3 is 90% of the cost", detail: "Tune this step first.", steps: [3], suggestion: null }];
  return { nodes, total_cost: 412, notes: ["dynamic statistics used: dynamic sampling (level=2)"], findings, text: "Plan hash value: 1234567890\n\n(demo output)" };
}

function ctxFor(q: string): AskContext {
  const words = q.toLowerCase().split(/\W+/).filter((w) => w.length > 2);
  const hit = Object.keys(TABLES).filter((t) => words.some((w) => t.toLowerCase().startsWith(w.replace(/s$/, "")) || TABLES[t].some((c) => c.toLowerCase().includes(w))));
  const names = hit.length ? hit : ["EMPLOYEES"];
  const text = `<schema owner="HR">\n${names.map((t) => `TABLE ${t} (${TABLES[t].join(", ")})`).join("\n")}\n</schema>`;
  return { text, objects: names.map((t) => ({ id: `TABLE:HR.${t}`, why: "name or column matches the question", score: 5 })), join_paths: ["EMPLOYEES.DEPARTMENT_ID → DEPARTMENTS.DEPARTMENT_ID"], tokens_estimate: Math.round(text.length / 4), truncated: false, excluded: 0 };
}

function validate(sql: string): SqlIssue[] {
  const out: SqlIssue[] = [];
  for (const m of sql.matchAll(/\b(?:from|join)\s+(?:hr\.)?([a-z_][\w$#]*)/gi)) {
    if (!TABLES[m[1].toUpperCase()]) out.push({ severity: "error", message: `table or view ${m[1].toUpperCase()} does not exist in HR` });
  }
  if (/\b(delete|update|insert|drop|truncate)\b/i.test(sql)) out.push({ severity: "warn", message: "this statement changes data or structure" });
  return out;
}

export function askApi(listeners: Set<(e: AiEvent) => void>): Pick<Api, "explainPlan" | "askContext" | "askStart" | "askValidate" | "askExtractSql" | "assistComplete" | "assistSuggest"> {
  let seq = 100;
  return {
    explainPlan: async (_c, sql) => { await new Promise((r) => setTimeout(r, 150)); return plan(sql); },
    askContext: async (_c, _o, input) => ctxFor(input.question),
    askStart: async (_c, _o, input) => {
      const id = `ask${++seq}`;
      const context = ctxFor(input.question);
      const reply = input.mode === "explain"
        ? "This reads EMPLOYEES and joins DEPARTMENTS on DEPARTMENT_ID. Filtering on UPPER(LAST_NAME) cannot use a plain index."
        : "Joins employees to their department through the foreign key.\n\n```sql\nSELECT e.last_name, d.department_name\nFROM hr.employees e\nJOIN hr.departments d ON d.department_id = e.department_id\nWHERE e.salary > :min_salary\nORDER BY e.last_name\n```\n\nAssumes you want every employee that has a department.";
      setTimeout(async () => {
        let text = "";
        for (const w of reply.split(/(?<= )/)) {
          text += w;
          listeners.forEach((l) => l({ type: "delta", id, text: w }));
          await new Promise((r) => setTimeout(r, 15));
        }
        listeners.forEach((l) => l({ type: "done", id, result: { text, usage: null, finish_reason: "stop", cancelled: false, elapsed_ms: 400 } }));
      }, 30);
      return { id, context };
    },
    askValidate: async (_c, _o, sql) => validate(sql),
    assistComplete: async (_c, _o, s) => {
      await new Promise((r) => setTimeout(r, 120));
      const b = s.before;
      const rules: [RegExp, string][] = [
        [/from\s+(?:hr\.)?emp$/i, "loyees e"],
        [/from\s+(?:hr\.)?employees\s+e\s*$/i, "JOIN hr.departments d ON d.department_id = e.department_id"],
        [/where\s*$/i, "e.department_id = :dept_id"],
        [/order\s+by\s*$/i, "e.last_name"],
      ];
      const hit = rules.find(([re]) => re.test(b));
      if (!hit || s.after.trim()) return { value: null, status: "skipped", message: null, sent: null };
      return { value: (b.endsWith(" ") && hit[1].startsWith(" ") ? hit[1].trimStart() : hit[1]), status: "ok", message: null, sent: [2, 60] };
    },
    assistSuggest: async (_c, _o, s, trigger) => {
      await new Promise((r) => setTimeout(r, 200));
      const out: AssistSuggestion[] = [];
      if (trigger === "error") out.push({ kind: "fix", title: "Table EMPLOYEE does not exist — did you mean HR.EMPLOYEES?", detail: "The error names a table that is not in this schema; the closest match is EMPLOYEES.", sql: (s.statement ?? "").replace(/\bemployee\b/gi, "hr.employees") || null });
      else if (trigger === "result") out.push({ kind: "next", title: `Count ${s.result?.rows ?? 0} rows per department`, detail: "A quick aggregate to see how the rows are spread.", sql: "SELECT department_id, COUNT(*) AS n FROM hr.employees GROUP BY department_id ORDER BY n DESC" });
      else if (/select\s+\*/i.test(s.statement ?? s.before)) out.push({ kind: "optimize", title: "SELECT * reads every column", detail: "List only the columns you need; it is faster and keeps the query stable when columns are added.", sql: null });
      return { value: out, status: "ok", message: null, sent: [2, 60] };
    },
    askExtractSql: async (answer) => [...answer.matchAll(/```sql\n([\s\S]*?)```/g)].map((m) => m[1].trim()),
  };
}
