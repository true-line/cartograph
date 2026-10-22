import { useState } from 'react';

export type McpStatus = 'disabled' | 'running' | 'stopped' | 'draining' | 'cleanup_failed' | 'unavailable';
export interface McpSettingsPanelProps {
  status: McpStatus;
  busy: boolean;
  available: boolean;
  error: string | null;
  configuration: string | null;
  onEnable: (policy: Record<string, unknown>) => void;
  onDisable: () => void;
  onRefresh: () => void;
  onConfiguration: () => void;
}
const policyShape = {
  transport: { max_connections: null, read_frame_bytes: null, write_frame_bytes: null, frame_deadline_ms: null, session_lifetime_ms: null },
  graph: { max_bytes: null, max_rows: null, max_depth: null, max_values: null },
  coverage: { max_records: null, max_bytes: null },
  dispatch: { max_response_bytes: null, max_request_ids: null, trace_max_flows: null, trace_max_hops: null, trace_max_output_bytes: null, compile_max_bytes: null, adr_max_rows: null, adr_max_output_bytes: null },
  evidence: { max_file_validation_bytes: null, max_span_bytes: null },
  aggregate: { max_workers: null, max_reserved_bytes: null, max_cumulative_read_bytes: null },
  worker_reserved_bytes: null,
  export_mode: 'verified-only',
};
export function McpSettingsPanel(props: McpSettingsPanelProps) {
  const [policyText, setPolicyText] = useState('');
  let policy: Record<string, unknown> | null = null;
  try {
    const parsed: unknown = JSON.parse(policyText);
    if (parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)) {
      policy = parsed as Record<string, unknown>;
    }
  } catch { /* The backend validates the complete required policy on submission. */ }
  const disabled = props.busy || !props.available;
  return <section className="settings-section" aria-label="Read-only MCP">
    <h3>Read-only MCP</h3>
    <p>Connect local agent clients to recovered context, artifacts, and retained evidence.
      Access starts disabled and ends when Cartograph closes. Connected clients control
      their own model and provider settings.</p>
    <p role="status">Read service: {props.status}</p>
    <details>
      <summary>Advanced resource policy</summary>
      <p>Supply an explicit JSON policy for this activation. No default policy is applied.
        The backend checks all required limits before opening access. Byte reservations are
        accounting limits, not a guarantee of process memory usage.</p>
      <details><summary>Required policy fields</summary>
        <p>Replace every null with a positive integer. Times are milliseconds and sizes are bytes.
          Export mode is verified-only or best-effort. This template supplies no numeric defaults.</p>
        <pre>{JSON.stringify(policyShape, null, 2)}</pre>
      </details>
      <label htmlFor="mcp-resource-policy">Resource policy JSON</label>
      <textarea id="mcp-resource-policy" rows={12} value={policyText}
        disabled={disabled || props.status === 'running' || props.status === 'draining'}
        onChange={(event) => setPolicyText(event.target.value)} />
      {policyText && !policy && <p>Enter a JSON object.</p>}
    </details>
    <div className="actions">
      <button disabled={disabled || props.status !== 'disabled' || !policy}
        onClick={() => { if (policy) props.onEnable(policy); }}>Enable read access</button>
      <button disabled={disabled || !['running', 'stopped'].includes(props.status)}
        onClick={props.onDisable}>Disable read access</button>
      <button disabled={disabled} onClick={props.onRefresh}>Refresh read service</button>
      <button disabled={disabled || props.status !== 'running'}
        onClick={props.onConfiguration}>Show client configuration</button>
    </div>
    {props.status === 'draining' && <p>Previous reads are finishing. Refresh before enabling a new session.</p>}
    {props.status === 'cleanup_failed' && <p role="alert">Read access was revoked, but endpoint cleanup could not be verified. New access is blocked.</p>}
    {props.error && <p role="alert">{props.error}</p>}
    {props.configuration && <>
      <p>Use this configuration in your local MCP client. Generate a new configuration after
        disabling or restarting Cartograph. Removing the client entry does not disable the service.</p>
      <pre aria-label="MCP client configuration">{props.configuration}</pre>
    </>}
  </section>;
}
