import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

/** One catalog model on the local lane (`LocalModelStatus` in the core). */
export interface LocalModelStatus {
  model: string;
  purpose: string;
  installed: boolean;
  /** The command that fetches the model; null when it is installed. */
  pull_command: string | null;
}

/** Whether the local model server answers and which catalog models it has. */
export interface LocalLaneStatus {
  reachable: boolean;
  /** The command that starts the server; null when it answered. */
  start_command: string | null;
  models: LocalModelStatus[];
}

export interface LocalLanePanelProps {
  /** `null` before the first check finishes. */
  status: LocalLaneStatus | null;
  checking: boolean;
  canCheck: boolean;
  error: string | null;
  onCheck: () => void;
}

/**
 * Settings → Local lane (#1795). Says whether the local model server is
 * running and which of the models Cartograph uses are installed, with the
 * command for each missing one. Cartograph never downloads a model itself.
 */
export function LocalLanePanel({ status, checking, canCheck, error, onCheck }: LocalLanePanelProps) {
  const missing = status?.models.filter((model) => !model.installed) ?? [];
  return (
    <section className="settings-surface" aria-label="Local lane">
      <h3 className="settings-section-title">Local lane</h3>
      <p className="muted">
        The local lane runs models through Ollama on this machine (127.0.0.1:11434). The check asks that server
        which models are installed; nothing leaves this machine. Cartograph never downloads a model itself.
      </p>
      {error && <p role="alert" className="error-text">{error}</p>}
      {!canCheck && <p className="muted">The local lane is checked by the core — connect a backend to check it.</p>}
      {canCheck && status === null && !error && <p className="muted">{checking ? 'Checking the local lane…' : 'Not checked yet.'}</p>}
      {status && !status.reachable && (
        <p role="status" data-testid="local-lane-summary">
          The local model server did not answer. Start it with <code>{status.start_command}</code>, then check again.
        </p>
      )}
      {status?.reachable && (
        <>
          <p role="status" data-testid="local-lane-summary">
            {missing.length === 0
              ? 'The local model server is running and every model is installed.'
              : `The local model server is running. ${missing.length} of ${status.models.length} models ${missing.length === 1 ? 'is' : 'are'} missing.`}
          </p>
          <ul className="tier-cards">
            {status.models.map((model) => (
              <li className="tier-card" key={model.model} data-testid={`local-model-${model.model}`}>
                <strong>{model.model}</strong> <span className="muted">{model.purpose}</span>
                <p className="muted">
                  {model.installed ? 'Installed.' : <>Missing. Run <code>{model.pull_command}</code></>}
                </p>
              </li>
            ))}
          </ul>
        </>
      )}
      <button type="button" className="secondary-button" disabled={!canCheck || checking} onClick={onCheck}>
        {checking ? 'Checking…' : 'Check local lane'}
      </button>
    </section>
  );
}

/** The panel bound to the core's `local_lane_status` command. */
export function LocalLane({ available }: { available: boolean }) {
  const [status, setStatus] = useState<LocalLaneStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Leaving the page drops a running check's result; the core stops it at its timeout.
  const generation = useRef(0);
  const probe = useCallback(() => {
    const ticket = ++generation.current;
    invoke<LocalLaneStatus>('local_lane_status').then(
      (next) => { if (ticket === generation.current) { setStatus(next); setError(null); } },
      (failure) => { if (ticket === generation.current) setError(String(failure)); },
    ).finally(() => { if (ticket === generation.current) setChecking(false); });
  }, []);
  // The first check runs when the page opens; until it answers the panel shows it as running.
  useEffect(() => {
    if (available) probe();
    return () => { generation.current += 1; };
  }, [available, probe]);
  const first = status === null && error === null;
  return <LocalLanePanel status={available ? status : null} checking={available && (checking || first)} canCheck={available}
    error={available ? error : null} onCheck={() => { setChecking(true); setError(null); probe(); }} />;
}
