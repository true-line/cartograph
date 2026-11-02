import { useEffect, useState } from 'react';
import { formatMinutes, parseMinutes, toMinutes, type RunLimits, type RunLimitsApi } from '../runLimits';

export interface ConfigureWindowProps {
  api: RunLimitsApi;
}

/**
 * Configure → Run Limits (#1633): the two investigation time limits, kept
 * apart from the app Settings page. Every other cap stays fixed.
 */
export function ConfigureWindow({ api }: ConfigureWindowProps) {
  const [limits, setLimits] = useState<RunLimits | null>(null);
  const [task, setTask] = useState('');
  const [call, setCall] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = (next: RunLimits) => {
    setLimits(next);
    setTask(String(toMinutes(next.active_seconds)));
    setCall(String(next.request_timeout_ms / 60_000));
  };

  useEffect(() => {
    api.load().then(show, (e: unknown) => setError(String(e)));
  }, [api]);

  const run = async (action: () => Promise<RunLimits>, message: string) => {
    setBusy(true); setError(null); setSaved(null);
    try { show(await action()); setSaved(message); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };

  if (!limits) {
    return <main className="configure-window" aria-label="Run limits">
      {error ? <>
        <p className="error-text" role="alert">{error}</p>
        <button type="button" disabled={busy}
          onClick={() => void run(() => api.reset(), 'Restored the defaults.')}>Restore defaults</button>
      </> : <p className="muted">Loading…</p>}
    </main>;
  }

  const taskMin = toMinutes(limits.default_active_seconds), taskMax = toMinutes(limits.max_active_seconds);
  const callMin = limits.default_request_timeout_ms / 60_000, callMax = limits.max_request_timeout_ms / 60_000;
  const taskValue = parseMinutes(task, taskMin, taskMax);
  const callValue = parseMinutes(call, callMin, callMax);
  const invalid = typeof taskValue === 'string' || typeof callValue === 'string';

  const save = () => {
    if (typeof taskValue === 'string' || typeof callValue === 'string') return;
    void run(() => api.save(Math.round(taskValue * 60), Math.round(callValue * 60_000)),
      'Saved. New investigations use these limits; running ones keep theirs.');
  };

  return <main className="configure-window" aria-label="Run limits">
    <h1>Run limits</h1>
    <p className="muted">How long an agent investigation may work before it stops. Raise these for slow local models.
      Other limits (model calls, actions, evidence) are fixed.</p>
    <form onSubmit={(event) => { event.preventDefault(); save(); }}>
      <label className="configure-field">
        <span>Task time limit (minutes)</span>
        <input type="number" inputMode="decimal" step="any" min={taskMin} max={taskMax} value={task}
          aria-invalid={typeof taskValue === 'string'} onChange={(event) => setTask(event.target.value)} />
        <small className={typeof taskValue === 'string' ? 'error-text' : 'muted'}>
          {typeof taskValue === 'string' ? taskValue : `Active working time for one investigation. Default ${formatMinutes(taskMin)}, up to ${formatMinutes(taskMax)}.`}
        </small>
      </label>
      <label className="configure-field">
        <span>Per model call timeout (minutes)</span>
        <input type="number" inputMode="decimal" step="any" min={callMin} max={callMax} value={call}
          aria-invalid={typeof callValue === 'string'} onChange={(event) => setCall(event.target.value)} />
        <small className={typeof callValue === 'string' ? 'error-text' : 'muted'}>
          {typeof callValue === 'string' ? callValue : `How long to wait for one model reply. Default ${formatMinutes(callMin)}, up to ${formatMinutes(callMax)}.`}
        </small>
      </label>
      <div className="configure-actions">
        <button type="submit" disabled={busy || invalid}>Save</button>
        <button type="button" className="secondary-button" disabled={busy || !limits.customized}
          onClick={() => void run(() => api.reset(), 'Restored the defaults.')}>Restore defaults</button>
      </div>
    </form>
    {saved && <p className="muted" role="status">{saved}</p>}
    {error && <p className="error-text" role="alert">{error}</p>}
  </main>;
}
