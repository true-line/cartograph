import { useEffect, useState } from 'react';
import { formatMinutes, parseMinutes, tauriLocalModelApi, toMinutes, type InstalledModel, type LocalModelApi,
  type LocalModelChoice, type RunLimits, type RunLimitsApi } from '../runLimits';

export interface ConfigureWindowProps {
  api: RunLimitsApi;
  modelApi?: LocalModelApi;
}

/** Configure → Local model (#1643): which installed Ollama model new desktop investigations use. */
function LocalModelSection({ api }: { api: LocalModelApi }) {
  const [choice, setChoice] = useState<LocalModelChoice | null>(null);
  const [installed, setInstalled] = useState<InstalledModel[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [selected, setSelected] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = (next: LocalModelChoice) => { setChoice(next); setSelected(next.model); };
  useEffect(() => {
    api.load().then(show, (e: unknown) => setError(String(e)));
    api.installed().then(setInstalled, (e: unknown) => setListError(String(e)));
  }, [api]);

  const run = async (action: () => Promise<LocalModelChoice>, message: string) => {
    setBusy(true); setError(null); setSaved(null);
    try { show(await action()); setSaved(message); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };

  const names = installed?.map((entry) => entry.name) ?? [];
  const options = choice && !names.includes(choice.model) ? [choice.model, ...names] : names;
  const digest = installed?.find((entry) => entry.name === selected)?.digest;

  return <section className="configure-section" aria-label="Local model">
    <h2>Local model</h2>
    <p className="muted">The Ollama model new desktop investigations on the local provider use. Cartograph never downloads a model;
      install one with <code>ollama pull</code> first. Running investigations keep their model.</p>
    {choice && <form onSubmit={(event) => { event.preventDefault(); void run(() => api.save(selected), `Saved. New investigations use ${selected}.`); }}>
      <label className="configure-field">
        <span>Completion model</span>
        <select value={selected} disabled={busy} onChange={(event) => setSelected(event.target.value)}>
          {options.map((name) => <option key={name} value={name}>
            {name}{name === choice.default_model ? ' (default)' : ''}{installed && !names.includes(name) ? ' (not installed)' : ''}</option>)}
        </select>
        <small className="muted">{listError ?? (digest ? `Installed digest ${digest.slice(0, 12)}.` : installed ? 'Not installed in Ollama on this Mac.' : 'Checking installed models…')}</small>
      </label>
      <div className="configure-actions">
        <button type="submit" disabled={busy || selected === choice.model}>Use this model</button>
        <button type="button" className="secondary-button" disabled={busy || !choice.customized}
          onClick={() => void run(() => api.reset(), `Restored the default model, ${choice.default_model}.`)}>Use default model</button>
      </div>
    </form>}
    {!choice && !error && <p className="muted">Loading…</p>}
    {!choice && error && <button type="button" disabled={busy}
      onClick={() => void run(() => api.reset(), 'Restored the default model.')}>Use default model</button>}
    {saved && <p className="muted" role="status">{saved}</p>}
    {error && <p className="error-text" role="alert">{error}</p>}
  </section>;
}

/**
 * Configure → Run Limits (#1633): the two investigation time limits, kept
 * apart from the app Settings page. Every other cap stays fixed.
 */
export function ConfigureWindow({ api, modelApi = tauriLocalModelApi }: ConfigureWindowProps) {
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
      They apply to new desktop investigations by the default (next-prompt) specialists; explicitly selected older versions keep the defaults.
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
    <LocalModelSection api={modelApi} />
  </main>;
}
