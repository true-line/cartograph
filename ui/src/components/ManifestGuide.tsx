import { useEffect, useRef, useState } from 'react';
import type { ManifestSuggestions } from '../manifestSuggestions';

export interface ManifestDraft {
  repos: { url: string }[];
  env: { key: string; value: string }[];
  services: { name: string; repo: string; base_urls: string[] }[];
  gateway_routes: { base_url: string; prefix: string; service: string; strip_prefix: boolean }[];
}
export interface ManifestGuideProps {
  preview: (draft: ManifestDraft) => Promise<string>;
  save: (draft: ManifestDraft) => Promise<string | null>;
  onSaved: (path: string) => void;
  /** Keys the last recovery's client calls could not resolve (#2474). */
  suggestions?: ManifestSuggestions;
}
const initial = (): ManifestDraft => ({ repos: [{ url: '' }], env: [], services: [], gateway_routes: [] });

/** New files only. Host validation owns TOML and binding semantics (AC-0002). */
export function ManifestGuide({ preview, save, onSaved, suggestions }: ManifestGuideProps) {
  const [draft, setDraft] = useState(initial);
  const [text, setText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const alive = useRef(true);
  const running = useRef(false);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const edit = (next: ManifestDraft) => { setDraft(next); setText(null); setMessage(''); };
  const run = async (saving: boolean) => {
    if (running.current) return;
    running.current = true;
    setBusy(true); setMessage('');
    // Keep editing text intact so Enter can start the next URL; normalize only
    // the submitted draft shared by preview and save (AC-0002).
    const submitted = {
      ...draft,
      services: draft.services.map(service => ({
        ...service,
        base_urls: service.base_urls.map(url => url.trim()).filter(url => url.length > 0),
      })),
    };
    try {
      if (saving) {
        const path = await save(submitted);
        if (!alive.current) return;
        if (path === null) setMessage('Save cancelled. Your draft is still here.');
        else if (typeof path !== 'string' || path.length === 0) { throw new Error('Save did not return a file path.'); }
        else { onSaved(path); setMessage('Manifest saved and selected. Close the guide, review the target, then run Preflight when ready.'); }
      } else {
        const rendered = await preview(submitted);
        if (typeof rendered !== 'string' || rendered.length === 0) throw new Error('Preview did not return a manifest.');
        if (alive.current) setText(rendered);
      }
    } catch (error) {
      if (alive.current) setMessage(String(error));
    } finally {
      running.current = false;
      if (alive.current) setBusy(false);
    }
  };
  return <section aria-label="Create system manifest" onKeyDown={(event) => {
    // This panel lives inside Connect's form. Enter must never start recovery/preflight.
    if (event.key === 'Enter' && event.target instanceof HTMLInputElement) event.preventDefault();
  }}>
    <h3>Create a system manifest</h3>
    <p>Declare the repositories and URLs you know. These declarations help link client calls; unresolved or ambiguous calls remain gaps.</p>
    <p>Relative repository paths are relative to the saved manifest. Configuration keys must match the source exactly, including case and punctuation. No repositories are read while drafting or saving.</p>
    {suggestions && (suggestions.keys.length > 0 || suggestions.computedSites.length > 0) && (
      <section aria-label="Suggested from the last recovery">
        <h4>Suggested from the last recovery</h4>
        {suggestions.keys.length > 0 && <>
          <p>These client calls build their URL from a key the recovery could not resolve. Add the key with its value, then declare the service that answers it.</p>
          <ul>
            {suggestions.keys.map(({ key, calls, sites }) => {
              const declared = draft.env.some(row => row.key === key);
              return <li key={key}>
                <code>{key}</code>: {calls} {calls === 1 ? 'call' : 'calls'} in {sites.slice(0, 3).join(', ')}{sites.length > 3 ? ` and ${sites.length - 3} more files` : ''}{' '}
                <button type="button" disabled={busy || declared} onClick={() => edit({ ...draft, env: [...draft.env, { key, value: '' }] })}>
                  {declared ? `${key} added` : `Add ${key}`}
                </button>
              </li>;
            })}
          </ul>
        </>}
        {suggestions.computedSites.length > 0 && (
          <p>{suggestions.computedSites.length} {suggestions.computedSites.length === 1 ? 'file builds' : 'files build'} a whole fetch URL at runtime ({suggestions.computedSites.slice(0, 3).join(', ')}{suggestions.computedSites.length > 3 ? ', …' : ''}). No manifest key can name these, so they stay gaps.</p>
        )}
      </section>
    )}
    <fieldset disabled={busy}>
      <legend>Repositories</legend>
      {draft.repos.map((repo, i) => <div key={i}>
        <label>Repository {i + 1}<input value={repo.url} onChange={e => edit({ ...draft, repos: draft.repos.map((r, n) => n === i ? { url: e.target.value } : r) })} /></label>
        <button type="button" onClick={() => edit({ ...draft, repos: draft.repos.filter((_, n) => n !== i) })}>Remove repository {i + 1}</button>
      </div>)}
      <button type="button" onClick={() => edit({ ...draft, repos: [...draft.repos, { url: '' }] })}>Add repository</button>
      <h4>Configuration</h4>
      {draft.env.map((row, i) => <div key={i}>
        <label>Configuration key {i + 1}<input value={row.key} onChange={e => edit({ ...draft, env: draft.env.map((r, n) => n === i ? { ...r, key: e.target.value } : r) })} /></label>
        <label>Configuration value {i + 1}<input value={row.value} onChange={e => edit({ ...draft, env: draft.env.map((r, n) => n === i ? { ...r, value: e.target.value } : r) })} /></label>
        <button type="button" onClick={() => edit({ ...draft, env: draft.env.filter((_, n) => n !== i) })}>Remove configuration {i + 1}</button>
      </div>)}
      <button type="button" onClick={() => edit({ ...draft, env: [...draft.env, { key: '', value: '' }] })}>Add configuration</button>
      <h4>Services</h4>
      {draft.services.map((row, i) => <div key={i}>
        <label>Service name {i + 1}<input value={row.name} onChange={e => edit({ ...draft, services: draft.services.map((r, n) => n === i ? { ...r, name: e.target.value } : r) })} /></label>
        <label>Service repository {i + 1}<select value={row.repo} onChange={e => edit({ ...draft, services: draft.services.map((r, n) => n === i ? { ...r, repo: e.target.value } : r) })}>
          <option value="">Choose repository</option>
          {draft.repos.map((r, n) => <option key={n} value={r.url}>{r.url || '(empty repository)'}</option>)}
          {row.repo && !draft.repos.some(r => r.url === row.repo) && <option value={row.repo}>{row.repo} (no longer declared)</option>}
        </select></label>
        <label>Service base URLs {i + 1} (one per line)<textarea value={row.base_urls.join('\n')} onChange={e => edit({ ...draft, services: draft.services.map((r, n) => n === i ? { ...r, base_urls: e.target.value.split('\n') } : r) })} /></label>
        <button type="button" onClick={() => edit({ ...draft, services: draft.services.filter((_, n) => n !== i) })}>Remove service {i + 1}</button>
      </div>)}
      <button type="button" onClick={() => edit({ ...draft, services: [...draft.services, { name: '', repo: '', base_urls: [] }] })}>Add service</button>
      <h4>Gateway routes (optional)</h4>
      {draft.gateway_routes.map((row, i) => <div key={i}>
        {(['base_url', 'prefix', 'service'] as const).map(key => <label key={key}>{({ base_url: 'Gateway base URL', prefix: 'Gateway prefix', service: 'Gateway service name' })[key]} {i + 1}<input value={row[key]} onChange={e => edit({ ...draft, gateway_routes: draft.gateway_routes.map((r, n) => n === i ? { ...r, [key]: e.target.value } : r) })} /></label>)}
        <label><input type="checkbox" checked={row.strip_prefix} onChange={e => edit({ ...draft, gateway_routes: draft.gateway_routes.map((r, n) => n === i ? { ...r, strip_prefix: e.target.checked } : r) })} />Strip gateway prefix {i + 1}</label>
        <button type="button" onClick={() => edit({ ...draft, gateway_routes: draft.gateway_routes.filter((_, n) => n !== i) })}>Remove gateway {i + 1}</button>
      </div>)}
      <button type="button" onClick={() => edit({ ...draft, gateway_routes: [...draft.gateway_routes, { base_url: '', prefix: '/', service: '', strip_prefix: false }] })}>Add gateway route</button>
      <p>Save to a new file. Existing manifests are never replaced.</p>
      <button type="button" onClick={() => void run(false)}>Preview manifest</button>
      <button type="button" disabled={text === null} onClick={() => void run(true)}>Save new manifest</button>
    </fieldset>
    {busy && <p role="status">{text === null ? 'Validating manifest…' : 'Waiting for save…'}</p>}
    {message && <p role="status">{message}</p>}
    {text !== null && <pre aria-label="Manifest preview">{text}</pre>}
  </section>;
}
