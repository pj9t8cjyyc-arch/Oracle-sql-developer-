import { useState } from "react";
import type { Analysis, Environment, Profile } from "../types";

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

const blank: Profile = { name: "", host: "", port: 1521, service: "", user: "", environment: "dev" };

export function ConnectDialog({ profiles, error, busy, onConnect, onSave }: {
  profiles: Profile[]; error: string; busy: boolean;
  onConnect(p: Profile, password: string): void; onSave(p: Profile): void;
}) {
  const [p, setP] = useState<Profile>(profiles[0] ?? blank);
  const [password, setPassword] = useState("");
  const set = <K extends keyof Profile>(k: K, v: Profile[K]) => setP({ ...p, [k]: v });
  const ok = p.name && p.host && p.service && p.user;
  return (
    <div className="modal-back">
      <form className="modal connect" onSubmit={(e) => { e.preventDefault(); if (ok) onConnect(p, password); }}>
        <h3>Connect to Oracle</h3>
        {profiles.length > 0 && (
          <select value={p.name} onChange={(e) => setP(profiles.find((x) => x.name === e.target.value) ?? blank)} aria-label="Saved connections">
            {profiles.map((x) => <option key={x.name}>{x.name}</option>)}
            <option value="">— new connection —</option>
          </select>
        )}
        <label>Name<input value={p.name} onChange={(e) => set("name", e.target.value)} /></label>
        <div className="row2">
          <label>Host<input value={p.host} onChange={(e) => set("host", e.target.value)} /></label>
          <label>Port<input type="number" value={p.port} onChange={(e) => set("port", Number(e.target.value))} /></label>
        </div>
        <label>Service name<input value={p.service} onChange={(e) => set("service", e.target.value)} /></label>
        <div className="row2">
          <label>User<input value={p.user} onChange={(e) => set("user", e.target.value)} autoComplete="off" /></label>
          <label>Password<input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="off" /></label>
        </div>
        <label>Environment
          <select value={p.environment} onChange={(e) => set("environment", e.target.value as Environment)}>
            <option value="dev">Development</option><option value="test">Test</option><option value="prod">PRODUCTION</option>
          </select>
        </label>
        {error && <div className="error">{error}</div>}
        <div className="actions">
          <button type="button" disabled={!ok} onClick={() => onSave(p)}>Save profile</button>
          <button type="submit" className="primary" disabled={!ok || busy}>{busy ? "Connecting…" : "Connect"}</button>
        </div>
        <p className="muted small">Passwords are kept in memory for this session only.</p>
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
