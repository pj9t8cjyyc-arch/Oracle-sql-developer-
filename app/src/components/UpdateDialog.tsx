import { useCallback, useEffect, useState } from "react";
import type { Api } from "../api";
import { errText } from "../hooks";
import { fmtBytes } from "../format";
import type { UpdateInfo, UpdateProgress, UpdateStatus } from "../types";

/** Version, update feed and "check now". Installing downloads, verifies the signature, installs and restarts. */
export function UpdateDialog({ api, found, onFound, onClose }: { api: Api; found?: UpdateInfo; onFound(u: UpdateInfo | undefined): void; onClose(): void }) {
  const [st, setSt] = useState<UpdateStatus>();
  const [endpoint, setEndpoint] = useState("");
  const [auto, setAuto] = useState(true);
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState<"check" | "install">();
  const [prog, setProg] = useState<UpdateProgress>();

  useEffect(() => { api.updateStatus().then((s) => { setSt(s); setAuto(s.auto_check); setEndpoint(s.custom_endpoint ? s.endpoint ?? "" : ""); }, (e) => setErr(errText(e))); }, [api]);
  useEffect(() => { let off = () => {}; let dead = false; api.onUpdateProgress(setProg).then((f) => { if (dead) f(); else off = f; }); return () => { dead = true; off(); }; }, [api]);

  const save = useCallback(async (): Promise<boolean> => {
    setErr("");
    try { const s = await api.updateSet(auto, endpoint.trim() || null); setSt(s); return true; } catch (e) { setErr(errText(e)); return false; }
  }, [api, auto, endpoint]);
  const check = async () => {
    if (!(await save())) return;
    setBusy("check"); setMsg(""); setErr("");
    try { const u = await api.updateCheck(); onFound(u ?? undefined); setMsg(u ? "" : "You are on the latest version."); } catch (e) { setErr(errText(e)); }
    finally { setBusy(undefined); }
  };
  const install = async () => {
    setBusy("install"); setErr(""); setProg(undefined);
    try { await api.updateInstall(); } catch (e) { setErr(errText(e)); setBusy(undefined); }
  };
  if (!st) return <div className="modal-back"><div className="modal">{err ? <div className="error">{err}</div> : "Loading…"}</div></div>;
  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && !busy && onClose()}>
      <div className="modal" role="dialog" aria-label="About and updates">
        <h3>Ora IDE <span className="muted">version {st.current_version}</span></h3>
        {!st.enabled ? <p className="muted">This build does not update itself. Install a release build (MSI on Windows, DMG on macOS) to get in-app updates.</p> : <>
          <label className="row"><input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} /> Check for updates when the app starts</label>
          <label className="col">Update address <span className="muted small">(leave empty to use the built-in one; an internal server must serve the same signed <code>latest.json</code>)</span>
            <input aria-label="Update address" value={endpoint} placeholder={st.endpoint ?? ""} onChange={(e) => setEndpoint(e.target.value)} /></label>
          {found && (
            <div className="upd-found" data-testid="update-found">
              <b>Version {found.version} is available</b>{found.date && <span className="muted small"> · {found.date}</span>}
              {found.notes && <p>{found.notes}</p>}
              <p className="muted small">The download is checked against the signing key built into this app before anything is installed. The app restarts afterwards; unsaved SQL in worksheets is kept.</p>
            </div>
          )}
          {busy === "install" && <div className="job-bar"><div className="job-fill" style={prog?.total ? { width: `${Math.min(100, (prog.downloaded / prog.total) * 100)}%`, animation: "none" } : undefined} /></div>}
          {busy === "install" && prog && <div className="muted small">{fmtBytes(prog.downloaded)}{prog.total ? ` of ${fmtBytes(prog.total)}` : ""}</div>}
          {msg && <div className="muted">{msg}</div>}
        </>}
        {err && <div className="error" role="alert">{err}</div>}
        <div className="modal-actions">
          <button onClick={onClose} disabled={busy === "install"}>Close</button>
          {st.enabled && <button onClick={() => void check()} disabled={!!busy}>{busy === "check" ? "Checking…" : "Check now"}</button>}
          {st.enabled && found && <button className="primary" onClick={() => void install()} disabled={!!busy}>{busy === "install" ? "Installing…" : "Install and restart"}</button>}
        </div>
      </div>
    </div>
  );
}
