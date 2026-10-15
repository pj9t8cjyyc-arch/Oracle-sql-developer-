import { useEffect, useState } from "react";
import type { Api } from "../api";
import { errText } from "../hooks";
import type { Analysis, AuthKind, ClientStatus, ConnKind, ConnTest, Environment, Profile, TnsList } from "../types";

export function ConfirmDialog({ analysis, onCancel, onRun }: { analysis: Analysis; onCancel(): void; onRun(): void }) {
  return (
    <div className="modal-back">
      <div className="modal" role="dialog" aria-label="Confirm statement">
        <h3>Confirm before running</h3>
        <pre className="sqlpreview">{analysis.sql}</pre>
        <ul className="warnings">
          {analysis.warnings.map((w, i) => <li key={i} className={w.severity}>{w.message}</li>)}
        </ul>
        <div className="actions">
          <button onClick={onCancel} autoFocus>Cancel</button>
          <button className="danger" onClick={onRun}>Run anyway</button>
        </div>
      </div>
    </div>
  );
}

const blank: Profile = { name: "", host: "", port: 1521, service: "", user: "", environment: "dev", kind: "easy", tls: false, tns_alias: "", tns_admin: "", connect_string: "", wallet_dir: "", auth: "password", proxy_target: "", privilege: "none", connect_timeout_secs: 20 };
const full = (p: Profile): Profile => ({ ...blank, ...p });

/** Can this profile be tried? (The same rules the core applies, so the buttons are honest.) */
function problem(p: Profile): string {
  if (!p.name.trim()) return "give the connection a name";
  if (p.kind === "easy" && (!p.host.trim() || !p.service.trim())) return "host and service are required";
  if (p.kind === "tns" && !p.tns_alias?.trim()) return "choose a TNS alias";
  if (p.kind === "connect_string" && !p.connect_string?.trim()) return "paste a connect string";
  if (p.auth !== "external" && !p.user.trim()) return "user is required";
  if (p.auth === "proxy" && !p.proxy_target?.trim()) return "the user to work as is required";
  return "";
}

export function ConnectDialog({ api, profiles, error, busy, onConnect, onSave, onCancel }: {
  api?: Api; profiles: Profile[]; error: string; busy: boolean;
  onConnect(p: Profile, password: string): void; onSave(p: Profile): void; onCancel?(): void;
}) {
  const [p, setP] = useState<Profile>(full(profiles[0] ?? blank));
  const [password, setPassword] = useState("");
  const [test, setTest] = useState<ConnTest>();
  const [testing, setTesting] = useState(false);
  const [aliases, setAliases] = useState<TnsList>();
  const [aliasErr, setAliasErr] = useState("");
  const [client, setClient] = useState<ClientStatus>();
  const set = <K extends keyof Profile>(k: K, v: Profile[K]) => { setP({ ...p, [k]: v }); setTest(undefined); };
  const why = problem(p);
  const advancedOpen = !!(p.wallet_dir || (p.privilege && p.privilege !== "none") || (p.connect_timeout_secs ?? 20) !== 20);

  useEffect(() => { api?.clientStatus().then(setClient, () => {}); }, [api]);

  const loadAliases = async (path = p.tns_admin ?? "") => {
    setAliasErr("");
    try { setAliases(await api!.tnsAliases(path)); } catch (e) { setAliases(undefined); setAliasErr(errText(e)); }
  };
  const browse = async () => {
    const f = await api?.pickOpenPath([]);
    if (f) { setP({ ...p, tns_admin: f }); setTest(undefined); await loadAliases(f); }
  };
  const runTest = async () => {
    if (!api || why) return;
    setTesting(true); setTest(undefined);
    try { setTest(await api.testConnection(p, password)); } catch (e) { setTest({ ok: false, error: errText(e), details: null, elapsed_ms: 0, client: null }); }
    finally { setTesting(false); }
  };
  const pick = (name: string) => {
    const x = profiles.find((q) => q.name === name);
    setP(full(x ?? blank)); setTest(undefined); setAliases(undefined);
  };

  return (
    <div className="modal-back">
      <form className="modal connect wide" onSubmit={(e) => { e.preventDefault(); if (!why) onConnect(p, password); }} aria-label="Connect to Oracle">
        <h3>Connect to Oracle</h3>
        {client && !client.ok && <div className="warn-box" role="alert"><b>Oracle Instant Client was not found.</b> {client.error}</div>}
        {profiles.length > 0 && (
          <select value={profiles.some((x) => x.name === p.name) ? p.name : ""} onChange={(e) => pick(e.target.value)} aria-label="Saved connections">
            {profiles.map((x) => <option key={x.name}>{x.name}</option>)}
            <option value="">— new connection —</option>
          </select>
        )}
        <label>Name<input value={p.name} onChange={(e) => set("name", e.target.value)} /></label>

        <div className="segmented tight" role="radiogroup" aria-label="How to reach the database">
          {([["easy", "Host & service"], ["tns", "TNS alias"], ["connect_string", "Connect string"]] as [ConnKind, string][]).map(([k, l]) => (
            <button type="button" key={k} role="radio" aria-checked={p.kind === k} className={p.kind === k ? "on" : ""} onClick={() => set("kind", k)}>{l}</button>
          ))}
        </div>
        {p.kind === "easy" && (
          <>
            <div className="row2">
              <label>Host<input value={p.host} onChange={(e) => set("host", e.target.value)} /></label>
              <label>Port<input type="number" value={p.port} onChange={(e) => set("port", Number(e.target.value))} /></label>
            </div>
            <label>Service name<input value={p.service} onChange={(e) => set("service", e.target.value)} /></label>
            <label className="check"><input type="checkbox" checked={!!p.tls} onChange={(e) => set("tls", e.target.checked)} /> Encrypt with TLS (tcps; usually port 2484 or 1522)</label>
          </>
        )}
        {p.kind === "tns" && (
          <>
            <div className="opt-row grow">
              <label className="grow">Folder or file with tnsnames.ora<input value={p.tns_admin ?? ""} onChange={(e) => set("tns_admin", e.target.value)} placeholder="empty = TNS_ADMIN / ORACLE_HOME" aria-label="tnsnames.ora location" /></label>
              <button type="button" onClick={browse} disabled={!api}>Browse…</button>
              <button type="button" onClick={() => loadAliases()} disabled={!api}>Load aliases</button>
            </div>
            {aliasErr && <div className="error small" role="alert">{aliasErr}</div>}
            <label>Alias
              {aliases ? (
                <select value={p.tns_alias ?? ""} onChange={(e) => set("tns_alias", e.target.value)} aria-label="TNS alias">
                  <option value="" disabled>choose an alias…</option>
                  {aliases.entries.flatMap((e) => e.names.map((n) => <option key={n} value={n}>{n}{e.host ? ` — ${e.host}${e.port ? `:${e.port}` : ""}${e.tls ? " (TLS)" : ""}` : ""}</option>))}
                </select>
              ) : <input value={p.tns_alias ?? ""} onChange={(e) => set("tns_alias", e.target.value)} placeholder="PROD" aria-label="TNS alias" />}
            </label>
            {aliases && <div className="muted small">{aliases.entries.length} alias entr{aliases.entries.length === 1 ? "y" : "ies"} in {aliases.file}</div>}
          </>
        )}
        {p.kind === "connect_string" && (
          <label>Connect string or full descriptor
            <textarea rows={3} spellCheck={false} value={p.connect_string ?? ""} onChange={(e) => set("connect_string", e.target.value.replace(/[\r\n]+/g, " "))} aria-label="Connect string"
              placeholder="host:1521/service?retry_count=3   or   (DESCRIPTION=(ADDRESS=(PROTOCOL=tcps)(HOST=…)(PORT=…))(CONNECT_DATA=(SERVICE_NAME=…)))" />
          </label>
        )}

        <div className="field-label">Sign in</div>
        <div className="segmented tight" role="radiogroup" aria-label="Sign-in method">
          {([["password", "User & password"], ["external", "Wallet / OS login"], ["proxy", "Proxy user"]] as [AuthKind, string][]).map(([k, l]) => (
            <button type="button" key={k} role="radio" aria-checked={p.auth === k} className={p.auth === k ? "on" : ""} onClick={() => set("auth", k)}>{l}</button>
          ))}
        </div>
        {p.auth === "external" ? (
          <p className="muted small">No user or password is needed here: the Oracle client takes the credentials from its wallet (sqlnet.ora <span className="mono">WALLET_LOCATION</span>) or from your operating-system login.</p>
        ) : (
          <div className="row2">
            <label>User<input value={p.user} onChange={(e) => set("user", e.target.value)} autoComplete="off" aria-label="User" /></label>
            <label>Password<input type="password" value={password} onChange={(e) => { setPassword(e.target.value); setTest(undefined); }} autoComplete="off" aria-label="Password" /></label>
          </div>
        )}
        {p.auth === "proxy" && <label>Work as (proxy target)<input value={p.proxy_target ?? ""} onChange={(e) => set("proxy_target", e.target.value)} placeholder="APP_OWNER" aria-label="Proxy target" /></label>}

        <details open={advancedOpen} className="adv">
          <summary>More options</summary>
          <div className="opt-row">
            <label>Wallet folder (TLS)<input value={p.wallet_dir ?? ""} onChange={(e) => set("wallet_dir", e.target.value)} placeholder="optional" aria-label="Wallet folder" /></label>
            <label>Connect as<select value={p.privilege ?? "none"} onChange={(e) => set("privilege", e.target.value as Profile["privilege"])} aria-label="Privilege" disabled={p.auth === "external"}>
              <option value="none">normal user</option><option value="sysdba">SYSDBA</option><option value="sysoper">SYSOPER</option></select></label>
            <label>Timeout (s)<input type="number" min={1} max={600} value={p.connect_timeout_secs ?? 20} onChange={(e) => set("connect_timeout_secs", Number(e.target.value))} aria-label="Timeout" /></label>
          </div>
          {p.privilege && p.privilege !== "none" && <div className="warn-box small">SYSDBA/SYSOPER sessions bypass most restrictions. Use them only when you must.</div>}
        </details>
        <label>Environment
          <select value={p.environment} onChange={(e) => set("environment", e.target.value as Environment)}>
            <option value="dev">Development</option><option value="test">Test</option><option value="prod">PRODUCTION</option>
          </select>
        </label>

        {test && (
          <div className={test.ok ? "ok-box" : "warn-box"} role="status" aria-label="Test result">
            {test.ok && test.details ? (
              <>
                <b>Connected</b> in {test.elapsed_ms} ms to <b>{test.details.banner || "an Oracle database"}</b>
                <div className="small">database {test.details.db_name}{test.details.container ? `, container ${test.details.container}` : ""} · signed in as {test.details.session_user}{test.details.server_host ? ` · server ${test.details.server_host}` : ""}</div>
                <div className="small">PL/Scope setting: {test.details.plscope_settings ?? "not readable by this user"}{test.client ? ` · Oracle client ${test.client}` : ""}</div>
              </>
            ) : <><b>Could not connect.</b><div className="small pre">{test.error}</div></>}
          </div>
        )}
        {error && <div className="error pre" role="alert">{error}</div>}
        <div className="actions">
          {onCancel && <button type="button" className="ghost" onClick={onCancel}>Cancel</button>}
          <button type="button" disabled={!!why} onClick={() => onSave(p)} title={why}>Save profile</button>
          <button type="button" disabled={!!why || testing || !api} onClick={runTest} title={why || "Connects, reads the database version, and disconnects"}>{testing ? "Testing…" : "Test connection"}</button>
          <button type="submit" className="primary" disabled={!!why || busy}>{busy ? "Connecting…" : "Connect"}</button>
        </div>
        <p className="muted small">Passwords are kept in memory for this session only. {why && <>To continue: {why}.</>}</p>
      </form>
    </div>
  );
}

export function SqlConfirmDialog({ title, sql, note, prod, busy, onCancel, onRun }: { title: string; sql: string; note: string; prod: boolean; busy: boolean; onCancel(): void; onRun(): void }) {
  return (
    <div className="modal-back">
      <div className="modal" role="dialog" aria-label={title}>
        <h3>{title}</h3>
        <pre className="sqlpreview">{sql}</pre>
        <ul className="warnings">
          <li className="warning">{note}</li>
          {prod && <li className="danger">You are connected to a PRODUCTION database.</li>}
        </ul>
        <div className="actions">
          <button onClick={onCancel} autoFocus>Cancel</button>
          <button className={prod ? "danger" : "primary"} disabled={busy} onClick={onRun}>{busy ? "Saving…" : "Run"}</button>
        </div>
      </div>
    </div>
  );
}
