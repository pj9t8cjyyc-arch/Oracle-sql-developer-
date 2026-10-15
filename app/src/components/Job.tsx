import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../api";
import { errText } from "../hooks";
import { fmtBytes } from "../format";
import type { JobDone, JobProgress } from "../types";

interface Seen { progress?: JobProgress; done?: JobDone }

/** Tracks one background job at a time. Events can arrive before `start` resolves, so they are kept per id. */
export function useJob(api: Api) {
  const seen = useRef(new Map<string, Seen>());
  const [id, setId] = useState<string>();
  const [, bump] = useState(0);
  const [startError, setStartError] = useState("");

  useEffect(() => {
    let off = () => {};
    let dead = false;
    api.onJob((e) => {
      const s = seen.current.get(e.data.id) ?? {};
      if (e.type === "progress") s.progress = e.data; else s.done = e.data;
      seen.current.set(e.data.id, s);
      bump((n) => n + 1);
    }).then((f) => { if (dead) f(); else off = f; });
    return () => { dead = true; off(); };
  }, [api]);

  const start = useCallback(async (run: () => Promise<string>) => {
    setStartError(""); setId(undefined);
    try { setId(await run()); } catch (e) { setStartError(errText(e)); }
  }, []);
  const reset = useCallback(() => { setId(undefined); setStartError(""); }, []);
  const cur = id ? seen.current.get(id) : undefined;
  return { id, progress: cur?.progress, done: cur?.done, startError, running: (id !== undefined && !cur?.done), start, reset };
}

export const fmtMs = (ms: number) => (ms < 1000 ? `${ms} ms` : ms < 60000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.floor(ms / 60000)} min ${Math.round((ms % 60000) / 1000)} s`);

export function JobBar({ progress, label, onCancel }: { progress?: JobProgress; label: string; onCancel(): void }) {
  const rate = progress && progress.elapsed_ms > 0 ? Math.round((progress.rows / progress.elapsed_ms) * 1000) : 0;
  return (
    <div className="job" role="status" aria-live="polite">
      <div className="job-bar"><div className="job-fill" /></div>
      <div className="job-line">
        <span>{label} <b>{(progress?.rows ?? 0).toLocaleString()}</b> rows</span>
        {progress && progress.bytes > 0 && <span className="muted">{fmtBytes(progress.bytes)}</span>}
        {progress && progress.rejected > 0 && <span className="warn">{progress.rejected.toLocaleString()} rejected</span>}
        {rate > 0 && <span className="muted">{rate.toLocaleString()} rows/s</span>}
        <span className="spacer" />
        <button onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}
