// Checks the demo's script splitter against the cases the Rust core is tested with.
import { readFileSync } from "node:fs";
import { sentSql, splitStatements } from "../src/split.ts";

const cases = JSON.parse(readFileSync(new URL("../../crates/ora-core/tests/fixtures/split_cases.json", import.meta.url), "utf8")) as { text: string; statements: { text: string; kind: string; sent: string }[] }[];
let bad = 0;
for (const c of cases) {
  const key = (x: { text: string; kind: string; sent: string }[]) => JSON.stringify(x.map((s) => [s.text, s.kind, s.sent]));
  const got = splitStatements(c.text).map((s) => ({ text: s.text, kind: s.kind, sent: sentSql(s.text) }));
  if (key(got) !== key(c.statements)) {
    bad++;
    console.error("MISMATCH for", JSON.stringify(c.text), "\n  expected", JSON.stringify(c.statements), "\n  got     ", JSON.stringify(got));
  }
}
if (bad) { console.error(`${bad} of ${cases.length} cases differ`); process.exit(1); }
console.log(`split parity: ${cases.length} cases match the Rust core`);
