import { useState } from 'react';
export type TaskOwnerStatus = {
  selected_client: string | null;
  task_dispatch_available: boolean;
  task_grant: { client_id: string; maximum_identities: number } | null;
};
export function positiveInteger(text: string): number | null {
  if (!/^[1-9]\d*$/.test(text)) return null;
  const value = Number(text);
  return Number.isSafeInteger(value) ? value : null;
}
export interface TaskGrantProps {
  owner: TaskOwnerStatus | null;
  clients: string[];
  selected: string;
  busy: boolean;
  canActivate: boolean;
  error: string | null;
  onSelect: (id: string) => void;
  onCreate: (limit: number) => void;
  onPage: (limit: number, next: boolean) => void;
  onGrant: (id: string, maximum: number) => void;
}
export function McpTaskGrantPanel(props: TaskGrantProps) {
  const [capacity, setCapacity] = useState('');
  const [pageSize, setPageSize] = useState('');
  const [identities, setIdentities] = useState('');
  const maximumClients = positiveInteger(capacity);
  const limit = positiveInteger(pageSize);
  const maximum = positiveInteger(identities);
  const disabled = props.busy || !props.owner?.task_dispatch_available;
  const options = [...new Set([...props.clients, props.selected].filter(Boolean))];
  return <section aria-label="Investigation access">
    <h4>Investigation access</h4>
    <p>Choose a registered client, then explicitly grant access to start and read its investigations.
      Creating or selecting a client does not grant access. Task requests use Cartograph’s provider
      settings and existing cloud consent. Disabling access does not cancel already-admitted work.</p>
    <p>Task grant: {!props.owner ? 'unavailable' : props.owner.task_grant
      ? `${props.owner.task_grant.client_id} · ${props.owner.task_grant.maximum_identities} request identities`
      : 'none'}</p>
    <label>Maximum registered clients<input value={capacity} inputMode="numeric" disabled={disabled}
      onChange={(event) => setCapacity(event.target.value)} /></label>
    <button disabled={disabled || !maximumClients} onClick={() => { if (maximumClients) props.onCreate(maximumClients); }}>Create client</button>
    <label>Clients per page<input value={pageSize} inputMode="numeric" disabled={disabled}
      onChange={(event) => setPageSize(event.target.value)} /></label>
    <button disabled={disabled || !limit} onClick={() => { if (limit) props.onPage(limit, false); }}>Load clients</button>
    <button disabled={disabled || !limit || !props.clients.length} onClick={() => { if (limit) props.onPage(limit, true); }}>Next clients</button>
    <label>Investigation client<select value={props.selected} disabled={disabled}
      onChange={(event) => props.onSelect(event.target.value)}>
      <option value="">Select a client</option>{options.map((id) => <option key={id} value={id}>{id}</option>)}
    </select></label>
    <label>Maximum request identities<input value={identities} inputMode="numeric" disabled={disabled}
      onChange={(event) => setIdentities(event.target.value)} /></label>
    <p>Supply positive integer limits. Request identities bound durable retry records; they are not a task concurrency setting.
      Stop an active service before granting access. Rotation removes the grant and requires explicit renewal.</p>
    <button disabled={disabled || !props.canActivate || !props.selected || !maximum}
      onClick={() => { if (maximum) props.onGrant(props.selected, maximum); }}>Enable investigation access</button>
    {props.error && <p role="alert">{props.error}</p>}
  </section>;
}
