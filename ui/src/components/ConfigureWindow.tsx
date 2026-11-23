import { useEffect, useState } from 'react';
import { CONTEXT_WINDOWS, DEFAULT_OLLAMA_PORT, formatContextWindow, formatMinutes, parseAgentArgs, parseMinutes, parsePort,
  tauriAcpAgentApi, tauriContextWindowApi, type AcpAgentApi, type AcpAgentCommand, tauriLocalModelApi, tauriOllamaPortApi, toMinutes, type ContextWindowApi, type InstalledModel,
  type LocalModelApi, type LocalModelChoice, type OllamaPortApi, type RunLimits, type RunLimitsApi } from '../runLimits';

export interface ConfigureWindowProps {
  api: RunLimitsApi;
  modelApi?: LocalModelApi;
  contextApi?: ContextWindowApi;
  portApi?: OllamaPortApi;
  agentApi?: AcpAgentApi;
}

/** Configure → ACP agent (AC-1350): the agent program reviewed work packets are dispatched to. */
function AcpAgentSection({ api }: { api: AcpAgentApi }) {
  const [current, setCurrent] = useState<AcpAgentCommand | null | undefined>(undefined);
  const [program, setProgram] = useState('');
  const [args, setArgs] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = (next: AcpAgentCommand | null) => {
    setCurrent(next); setProgram(next?.program ?? ''); setArgs(next?.args.join('\n') ?? '');
  };
  useEffect(() => { api.load().then(show, (e: unknown) => setError(String(e))); }, [api]);

  const save = async (command: AcpAgentCommand | null) => {
    setBusy(true); setError(null); setSaved(null);
    try {
      const next = await api.save(command);
      show(next);
      setSaved(next ? `Saved. Reviewed packets are dispatched to ${next.program}.` : 'Cleared. No packet can be dispatched until an agent is set.');
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const trimmed = program.trim();

  return <section className="configure-section" aria-label="ACP agent">
    <h2>ACP agent</h2>
    <p className="muted">The Agent Client Protocol program a reviewed work packet is dispatched to. It runs on this Mac in its own
      worktree, may read and edit only inside it, and its work comes back as a local branch; nothing is pushed or merged.</p>
    {current !== undefined && <form onSubmit={(event) => {
      event.preventDefault();
      if (trimmed) void save({ program: trimmed, args: parseAgentArgs(args) });
    }}>
      <label className="configure-field">
        <span>Program (absolute path)</span>
        <input type="text" spellCheck={false} value={program} disabled={busy} maxLength={1024}
          onChange={(event) => setProgram(event.target.value)} />
      </label>
      <label className="configure-field">
        <span>Arguments (one per line)</span>
        <textarea rows={3} spellCheck={false} value={args} disabled={busy} onChange={(event) => setArgs(event.target.value)} />
        <small className="muted">At most 32 arguments. No shell is involved, so quote nothing.</small>
      </label>
      <div className="configure-actions">
        <button type="submit" disabled={busy || !trimmed}>Use this agent</button>
        <button type="button" className="secondary-button" disabled={busy || current === null}
          onClick={() => void save(null)}>Clear agent</button>
      </div>
    </form>}
    {current === undefined && !error && <p className="muted">Loading…</p>}
    {saved && <p className="muted" role="status">{saved}</p>}
    {error && <p className="error-text" role="alert">{error}</p>}
  </section>;
}

/** Configure → Ollama port (#1653): the loopback port new desktop investigations reach Ollama on. */
function OllamaPortSection({ api }: { api: OllamaPortApi }) {
  const [current, setCurrent] = useState<number | null | undefined>(undefined);
  const [text, setText] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = (next: number | null) => { setCurrent(next); setText(String(next ?? DEFAULT_OLLAMA_PORT)); };
  useEffect(() => { api.load().then(show, (e: unknown) => setError(String(e))); }, [api]);

  const save = async (port: number | null) => {
    setBusy(true); setError(null); setSaved(null);
    try {
      const next = await api.save(port);
      show(next);
      setSaved(`Saved. New investigations reach Ollama at 127.0.0.1:${next ?? DEFAULT_OLLAMA_PORT}. Reopen this window to list its models.`);
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const parsed = parsePort(text);
  const unchanged = typeof parsed === 'number' && parsed === (current ?? DEFAULT_OLLAMA_PORT);

  return <section className="configure-section" aria-label="Ollama port">
    <h2>Ollama port</h2>
    <p className="muted">The port Ollama listens on, on this Mac only (127.0.0.1). Change it only if you run Ollama on a
      port other than {DEFAULT_OLLAMA_PORT}. Running investigations keep theirs, and the endpoint is recorded with each run.</p>
    {current !== undefined && <form onSubmit={(event) => {
      event.preventDefault();
      if (typeof parsed === 'number') void save(parsed);
    }}>
      <label className="configure-field">
        <span>Port</span>
        <input type="number" inputMode="numeric" min={1} max={65535} step={1} value={text} disabled={busy}
          aria-invalid={typeof parsed === 'string'} onChange={(event) => setText(event.target.value)} />
        <small className={typeof parsed === 'string' ? 'error-text' : 'muted'}>
          {typeof parsed === 'string' ? parsed : `Default ${DEFAULT_OLLAMA_PORT}.`}
        </small>
      </label>
      <div className="configure-actions">
        <button type="submit" disabled={busy || typeof parsed === 'string' || unchanged}>Use this port</button>
        <button type="button" className="secondary-button" disabled={busy || current === null}
          onClick={() => void save(null)}>Use default port</button>
      </div>
    </form>}
    {current === undefined && !error && <p className="muted">Loading…</p>}
    {current === undefined && error && <button type="button" disabled={busy}
      onClick={() => void save(null)}>Use default port</button>}
    {saved && <p className="muted" role="status">{saved}</p>}
    {error && <p className="error-text" role="alert">{error}</p>}
  </section>;
}

/** Configure → Context window (#1653): the `num_ctx` new desktop investigations ask Ollama for. */
function ContextWindowSection({ api }: { api: ContextWindowApi }) {
  const [current, setCurrent] = useState<number | null | undefined>(undefined);
  const [selected, setSelected] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const show = (next: number | null) => { setCurrent(next); setSelected(next === null ? '' : String(next)); };
  useEffect(() => { api.load().then(show, (e: unknown) => setError(String(e))); }, [api]);

  const save = async (tokens: number | null) => {
    setBusy(true); setError(null); setSaved(null);
    try {
      const next = await api.save(tokens);
      show(next);
      setSaved(next === null ? "Saved. New investigations leave the context window to Ollama."
        : `Saved. New investigations ask Ollama for ${formatContextWindow(next)}.`);
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const chosen = selected === '' ? null : Number(selected);

  return <section className="configure-section" aria-label="Context window">
    <h2>Context window</h2>
    <p className="muted">How much text a local model reads per call. Ollama's own default can silently cut a long prompt short;
      a larger window uses more memory on this Mac. Running investigations keep theirs, and the choice is recorded with each run.</p>
    {current !== undefined && <form onSubmit={(event) => { event.preventDefault(); void save(chosen); }}>
      <label className="configure-field">
        <span>Context window</span>
        <select value={selected} disabled={busy} onChange={(event) => setSelected(event.target.value)}>
          <option value="">Ollama default</option>
          {CONTEXT_WINDOWS.map((tokens) => <option key={tokens} value={String(tokens)}>{formatContextWindow(tokens)}</option>)}
        </select>
      </label>
      <div className="configure-actions">
        <button type="submit" disabled={busy || chosen === current}>Use this window</button>
        <button type="button" className="secondary-button" disabled={busy || current === null}
          onClick={() => void save(null)}>Use Ollama default</button>
      </div>
    </form>}
    {current === undefined && !error && <p className="muted">Loading…</p>}
    {current === undefined && error && <button type="button" disabled={busy}
      onClick={() => void save(null)}>Use Ollama default</button>}
    {saved && <p className="muted" role="status">{saved}</p>}
    {error && <p className="error-text" role="alert">{error}</p>}
  </section>;
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
export function ConfigureWindow({ api, modelApi = tauriLocalModelApi, contextApi = tauriContextWindowApi,
  portApi = tauriOllamaPortApi, agentApi = tauriAcpAgentApi }: ConfigureWindowProps) {
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
    <ContextWindowSection api={contextApi} />
    <OllamaPortSection api={portApi} />
    <AcpAgentSection api={agentApi} />
  </main>;
}
