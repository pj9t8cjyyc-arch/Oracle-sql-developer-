import { useCallback, useEffect, useRef, useState } from "react";

export const errText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));

/** Run an async loader whenever `deps` change; ignores results of superseded runs. */
export function useAsync<T>(fn: () => Promise<T>, deps: unknown[]): { data: T | undefined; error: string; loading: boolean; reload(): void } {
  const [data, setData] = useState<T>();
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const seq = useRef(0);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  useEffect(() => {
    const id = ++seq.current;
    setLoading(true);
    setError("");
    fnRef.current().then(
      (d) => { if (id === seq.current) { setData(d); setLoading(false); } },
      (e) => { if (id === seq.current) { setError(errText(e)); setLoading(false); } },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { data, error, loading, reload };
}
