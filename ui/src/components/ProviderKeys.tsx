import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

/** Where the key a cloud call would use comes from. Never the key (#1763). */
export interface ProviderKeyStatus {
  provider: string;
  label: string;
  env_var: string;
  source: 'environment' | 'stored' | 'none';
  /** A key is saved in the system credential store. */
  stored: boolean;
  /** The credential store could not be read. */
  store_error: string | null;
}

/** What a connection check found (#1795). Never the key or a response body. */
export interface ConnectionCheck {
  provider: string;
  outcome: 'accepted' | 'refused' | 'unreachable' | 'no-key';
  /** Bytes written to the egress record for the check. */
  bytes_sent: number;
}

/** A provider's check: running, finished, or failed before anything was sent. */
export type ConnectionCheckState = 'checking' | ConnectionCheck | { error: string };

function describeCheck(check: ConnectionCheckState): string {
  if (check === 'checking') return 'Checking…';
  if ('error' in check) return `The check did not run: ${check.error}`;
  const recorded = `${check.bytes_sent} bytes were sent and added to the egress record.`;
  if (check.outcome === 'accepted') return `The provider accepted the key. ${recorded}`;
  if (check.outcome === 'refused') return `The provider refused the key. ${recorded}`;
  if (check.outcome === 'unreachable') return `The provider could not be reached, so the key is unconfirmed. ${recorded}`;
  return 'There is no key to check. Nothing was sent.';
}

export interface ProviderKeysPanelProps {
  /** `null` while loading or with no backend. */
  statuses: ProviderKeyStatus[] | null;
  canEdit: boolean;
  busy: boolean;
  error: string | null;
  onSave: (provider: string, key: string) => void;
  onRemove: (provider: string) => void;
  /** The latest check per provider id, if one was started. */
  checks: Record<string, ConnectionCheckState>;
  onCheck: (provider: string) => void;
}

function describe(status: ProviderKeyStatus): string {
  if (status.source === 'environment') {
    return `Using the ${status.env_var} environment variable.` +
      (status.stored ? ' A saved key is also present; the variable takes precedence.' : '');
  }
  if (status.source === 'stored') return 'A key is saved in the system credential store.';
  return 'No key. The cloud lane stays closed.';
}

function ProviderKeyRow({ status, canEdit, busy, onSave, onRemove, check, onCheck }: {
  status: ProviderKeyStatus; canEdit: boolean; busy: boolean;
  onSave: (provider: string, key: string) => void; onRemove: (provider: string) => void;
  check: ConnectionCheckState | undefined; onCheck: (provider: string) => void;
}) {
  // The typed key lives only in this input until it is sent to the core; it
  // is cleared on submit and never read back.
  const [draft, setDraft] = useState('');
  const inputId = `provider-key-${status.provider}`;
  return (
    <li className="tier-card">
      <strong>{status.label}</strong>
      <p className="muted" data-testid={`provider-key-status-${status.provider}`}>{describe(status)}</p>
      {status.store_error && <p role="alert" className="error-text">The credential store could not be read: {status.store_error}</p>}
      <form onSubmit={(event) => { event.preventDefault(); const key = draft; setDraft(''); onSave(status.provider, key); }}>
        <label htmlFor={inputId}>{status.stored ? 'Replace key' : 'API key'}</label>
        <input id={inputId} type="password" autoComplete="off" spellCheck={false} value={draft}
          disabled={!canEdit || busy} maxLength={512} onChange={(event) => setDraft(event.target.value)} />
        <button type="submit" disabled={!canEdit || busy || draft.trim() === ''}>{status.stored ? 'Replace key' : 'Save key'}</button>
        {status.stored && <button type="button" className="secondary-button" disabled={!canEdit || busy}
          onClick={() => onRemove(status.provider)}>Remove key</button>}
      </form>
      <button type="button" className="secondary-button"
        disabled={!canEdit || busy || check === 'checking' || status.source === 'none'}
        onClick={() => onCheck(status.provider)}>Check connection</button>
      {check && <p role="status" className="muted" data-testid={`provider-check-${status.provider}`}>{describeCheck(check)}</p>}
    </li>
  );
}

/**
 * Settings → Provider keys (#1763). Shows whether a key is present and where
 * it comes from, and lets the user enter, replace or remove one. The key is
 * write-only: the core stores it in the system credential store and never
 * sends it back.
 */
export function ProviderKeysPanel({ statuses, canEdit, busy, error, onSave, onRemove, checks, onCheck }: ProviderKeysPanelProps) {
  return (
    <section className="settings-surface" aria-label="Provider keys">
      <h3 className="settings-section-title">Provider keys</h3>
      <p className="muted">
        A cloud provider needs an API key. Cartograph keeps it in the system credential store
        (Keychain on macOS, Credential Manager on Windows) and nowhere else. Saving a key sends
        nothing: each cloud step still shows what would leave this machine and asks for consent.
      </p>
      <p className="muted">
        <strong>Check connection</strong> sends one request for the provider's model list, with your key and
        nothing else: no source text, no prompt and no graph content. It needs no consent, and its size is
        added to the egress record. The check stops after 10 seconds.
      </p>
      {error && <p role="alert" className="error-text">{error}</p>}
      {statuses === null
        ? <p className="muted">{canEdit ? 'Checking for saved keys…' : 'Provider keys live in the core — connect a backend to change them.'}</p>
        : <ul className="tier-cards">{statuses.map((status) => (
          <ProviderKeyRow key={status.provider} status={status} canEdit={canEdit} busy={busy} onSave={onSave} onRemove={onRemove}
            check={checks[status.provider]} onCheck={onCheck} />
        ))}</ul>}
    </section>
  );
}

/** The panel bound to the core's three provider-key commands. */
export function ProviderKeys({ available }: { available: boolean }) {
  const [statuses, setStatuses] = useState<ProviderKeyStatus[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [checks, setChecks] = useState<Record<string, ConnectionCheckState>>({});
  // Leaving the page drops a running check's result; the core stops it at its timeout.
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const check = (provider: string) => {
    setChecks((current) => ({ ...current, [provider]: 'checking' }));
    invoke<ConnectionCheck>('check_provider_connection', { provider }).then(
      (result) => { if (mounted.current) setChecks((current) => ({ ...current, [provider]: result })); },
      (failure) => { if (mounted.current) setChecks((current) => ({ ...current, [provider]: { error: String(failure) } })); },
    );
  };
  // A changed key makes an earlier result stale.
  const forgetCheck = (provider: string) => setChecks((current) =>
    Object.fromEntries(Object.entries(current).filter(([id]) => id !== provider)));
  const generation = useRef(0);
  useEffect(() => {
    const ticket = ++generation.current;
    if (!available) return;
    invoke<ProviderKeyStatus[]>('provider_key_status').then(
      (next) => { if (ticket === generation.current) setStatuses(next); },
      (failure) => { if (ticket === generation.current) setError(String(failure)); },
    );
    return () => { generation.current += 1; };
  }, [available]);
  const change = (operation: Promise<ProviderKeyStatus>) => {
    const ticket = ++generation.current;
    setBusy(true); setError(null);
    operation.then(
      (next) => {
        if (ticket !== generation.current) return;
        setStatuses((current) => (current ?? []).map((status) => status.provider === next.provider ? next : status));
      },
      (failure) => { if (ticket === generation.current) setError(String(failure)); },
    ).finally(() => { if (ticket === generation.current) setBusy(false); });
  };
  return <ProviderKeysPanel statuses={available ? statuses : null} canEdit={available} busy={busy} error={available ? error : null}
    checks={available ? checks : {}} onCheck={check}
    onSave={(provider, key) => { forgetCheck(provider); change(invoke<ProviderKeyStatus>('set_provider_key', { provider, key })); }}
    onRemove={(provider) => { forgetCheck(provider); change(invoke<ProviderKeyStatus>('remove_provider_key', { provider })); }} />;
}
