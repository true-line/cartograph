import { useEffect, useRef, useState, type ComponentProps } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { McpTaskGrantPanel, type TaskOwnerStatus } from './McpTaskGrantPanel';
import type { McpStatus } from './McpSettingsPanel';

function ownerStatus(value: unknown): TaskOwnerStatus {
  if (!value || typeof value !== 'object') throw new Error('invalid owner status');
  const row = value as TaskOwnerStatus;
  if (typeof row.task_dispatch_available !== 'boolean'
    || !(row.selected_client === null || typeof row.selected_client === 'string')
    || !(row.task_grant === null || (row.task_grant && typeof row.task_grant.client_id === 'string'
      && Number.isSafeInteger(row.task_grant.maximum_identities) && row.task_grant.maximum_identities > 0))) {
    throw new Error('invalid owner status');
  }
  return row;
}
export function McpTaskSettings(props: ComponentProps<typeof McpTaskSettingsSession>) {
  return <McpTaskSettingsSession key={`${props.available}:${props.serviceBusy}:${props.status}`} {...props} />;
}
function McpTaskSettingsSession(props: {
  available: boolean; serviceBusy: boolean; status: McpStatus; policy: Record<string, unknown> | null;
  onGrant: (policy: Record<string, unknown>, clientId: string, maximumIdentities: number) => void;
}) {
  const [owner, setOwner] = useState<TaskOwnerStatus | null>(null);
  const [clients, setClients] = useState<string[]>([]);
  const [selected, setSelected] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  useEffect(() => {
    const ticket = ++generation.current;
    if (props.available && !props.serviceBusy && props.status !== 'unavailable') {
      void invoke<unknown>('mcp_reads_owner_status').then((value) => {
        const observed = ownerStatus(value);
        if (ticket !== generation.current) return;
        setOwner(observed);
        if (observed.selected_client) setSelected(observed.selected_client);
      }).catch(() => { if (ticket === generation.current) setOwner(null); });
    }
    return () => { generation.current += 1; };
  }, [props.available, props.serviceBusy, props.status]);
  const registry = async (operation: () => Promise<unknown>, created: boolean) => {
    const ticket = ++generation.current;
    setBusy(true); setError(null);
    try {
      const value = await operation();
      if (ticket !== generation.current) return;
      if (created) {
        if (typeof value !== 'string' || !value) throw new Error('invalid client');
        setSelected(value);
      } else {
        if (!Array.isArray(value) || !value.every((id) => typeof id === 'string' && id)) throw new Error('invalid clients');
        setClients(value);
      }
    } catch {
      if (ticket === generation.current) setError('Client registration could not be updated. Refresh the service and try again.');
    } finally { if (ticket === generation.current) setBusy(false); }
  };
  return <McpTaskGrantPanel owner={props.serviceBusy ? null : owner} clients={clients} selected={selected}
    busy={busy || props.serviceBusy || !props.available} error={error} onSelect={setSelected}
    canActivate={props.status === 'disabled' && !!props.policy}
    onCreate={(maximumClients) => void registry(() => invoke('mcp_task_client_create', { maximumClients }), true)}
    onPage={(limit, next) => void registry(() => invoke('mcp_task_clients', { limit, after: next ? clients.at(-1) ?? null : null }), false)}
    onGrant={(client, maximum) => { if (props.policy) props.onGrant(props.policy, client, maximum); }} />;
}
