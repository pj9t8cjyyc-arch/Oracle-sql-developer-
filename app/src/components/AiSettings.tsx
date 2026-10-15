import { useEffect, useState } from "react";
import type { Api } from "../api";
import { errText } from "../hooks";
import type { AiConfig, AiStatus, AiTest } from "../types";

const headersToText = (h: Record<string, string>) => Object.entries(h).map(([k, v]) => `${k}: ${v}`).join("\n");
const textToHeaders = (t: string): Record<string, string> =>
  Object.fromEntries(t.split("\n").map((l) => l.trim()).filter(Boolean).map((l) => { const i = l.indexOf(":"); return i < 0 ? [l, ""] : [l.slice(0, i).trim(), l.slice(i + 1).trim()]; }));

export function AiSettings({ api, onClose, onChanged }: { api: Api; onClose(): void; onChanged(s: AiStatus): void }) {
  const [cfg, setCfg] = useState<AiConfig>();
  const [headers, setHeaders] = useState("");
  const [status, setStatus] = useState<AiStatus>();
  const [token, setToken] = useState("");
  const [test, setTest] = useState<AiTest>();
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    Promise.all([api.aiGetConfig(), api.aiStatus()]).then(([c, s]) => { setCfg(c); setHeaders(headersToText(c.extra_headers)); setStatus(s); }, (e) => setErr(errText(e)));
  }, [api]);
  if (!cfg) return <div className="modal-back"><div className="modal">{err ? <div className="error">{err}</div> : "Loading…"}</div></div>;
  const set = <K extends keyof AiConfig>(k: K, v: AiConfig[K]) => setCfg({ ...cfg, [k]: v });
  const update = (s: AiStatus) => { setStatus(s); onChanged(s); };
  const mock = cfg.provider === "mock";

  const save = async (): Promise<boolean> => {
    setErr("");
    try { update(await api.aiSaveConfig({ ...cfg, extra_headers: textToHeaders(headers) })); return true; } catch (e) { setErr(errText(e)); return false; }
  };
  const run = async (f: () => Promise<void>) => { setBusy(true); try { await f(); } catch (e) { setErr(errText(e)); } finally { setBusy(false); } };

  return (
    <div className="modal-back">
      <div className="modal wide ai" role="dialog" aria-label="AI settings">
        <h3>AI connection</h3>
        {status && (
          <div className={status.signed_in && status.configured ? "ok-box" : "warn-box"} role="status" aria-label="AI status">
            {!status.configured ? `Not set up: ${status.config_error ?? "choose a provider"}` : status.signed_in ? `Ready: signed in${status.expires_at_ms ? `, token valid until ${new Date(status.expires_at_ms).toLocaleTimeString()}` : ""}.` : "Set up, but not signed in: no token. Sign in with your enterprise login."}
          </div>
        )}
        <div className="opt-row">
          <label>Provider<select value={cfg.provider} onChange={(e) => set("provider", e.target.value as AiConfig["provider"])}>
            <option value="openai_compatible">OpenAI-compatible (chat completions)</option><option value="mock">Demo (no network)</option></select></label>
        </div>
        {!mock && (
          <>
            <div className="opt-row">
              <label className="grow">Endpoint<input value={cfg.base_url} onChange={(e) => set("base_url", e.target.value)} placeholder="https://ai.your-company.com/v1" aria-label="Endpoint" /></label>
              <label>Path<input value={cfg.path} onChange={(e) => set("path", e.target.value)} aria-label="Path" /></label>
              <label>Model<input value={cfg.model} onChange={(e) => set("model", e.target.value)} aria-label="Model" /></label>
            </div>
            <div className="opt-row">
              <label>Token header<input value={cfg.auth_header} onChange={(e) => set("auth_header", e.target.value)} aria-label="Token header" /></label>
              <label>Prefix<input value={cfg.auth_scheme} onChange={(e) => set("auth_scheme", e.target.value)} placeholder="Bearer (empty = token only)" aria-label="Token prefix" /></label>
              <label>Timeout (s)<input type="number" min={1} max={3600} value={cfg.timeout_secs} onChange={(e) => set("timeout_secs", Number(e.target.value))} /></label>
              <label className="check"><input type="checkbox" checked={cfg.stream} onChange={(e) => set("stream", e.target.checked)} /> Stream the answer</label>
            </div>
            <label>Extra headers (one per line, <span className="mono">Name: value</span>; not for secrets)
              <textarea rows={2} value={headers} onChange={(e) => setHeaders(e.target.value)} aria-label="Extra headers" /></label>
          </>
        )}
        <div className="field-label">Sign-in</div>
        <p className="muted small">
          Your enterprise login gives the app a token by calling <span className="mono">provideAiToken(api, token, expiresAtMs)</span> (see <span className="mono">src/ai-login.ts</span>).
          The token stays in memory: it is not saved, not shown, and not sent anywhere except to the endpoint above.
        </p>
        <div className="opt-row grow">
          <label className="grow">Paste a token (for testing only)<input type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value)} aria-label="Token" /></label>
          <button disabled={!token.trim() || busy} onClick={() => run(async () => { update(await api.aiSetToken(token.trim(), null)); setToken(""); })}>Use token</button>
          <button disabled={!status?.signed_in || busy} onClick={() => run(async () => update(await api.aiClearToken()))}>Sign out</button>
        </div>
        {test && <div className={test.ok ? "ok-box" : "warn-box"} role="status" aria-label="Test result">{test.ok ? `Works: the service answered "${test.text}" in ${test.elapsed_ms} ms.` : `Failed: ${test.error}`}{test.auth_required && " Sign in again."}</div>}
        {err && <div className="error" role="alert">{err}</div>}
        <div className="actions">
          <button onClick={onClose}>Close</button>
          <button disabled={busy} onClick={() => run(async () => { if (await save()) setTest(await api.aiTest()); })} title="Saves the settings and sends one tiny message">Save and test</button>
          <button className="primary" disabled={busy} onClick={() => run(async () => { if (await save()) onClose(); })}>Save</button>
        </div>
      </div>
    </div>
  );
}
