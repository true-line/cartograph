import { useState } from 'react';

export type McpStatus = 'disabled' | 'running' | 'stopped' | 'draining' | 'cleanup_failed' | 'unavailable';
export type McpClientSkill = { status: 'unavailable' } | {
  status: 'available'; app_version: string; skill_path: string;
  files: { path: string; content: string }[];
};
export interface McpSettingsPanelProps {
  status: McpStatus;
  busy: boolean;
  available: boolean;
  error: string | null;
  configuration: string | null;
  skill: McpClientSkill | null;
  skillBusy: boolean;
  onSkill: () => void;
  onEnable: (policy: Record<string, unknown>) => void;
  onDisable: () => void;
  onRotate: () => void;
  onRefresh: () => void;
  onConfiguration: () => void;
}
const policyShape = {
  acquisition_mode: 'complete',
  transport: { max_connections: null, read_frame_bytes: null, write_frame_bytes: null, frame_deadline_ms: null, session_lifetime_ms: null },
  graph: { max_bytes: null, max_rows: null, max_depth: null, max_values: null },
  coverage: { max_records: null, max_bytes: null },
  dispatch: { max_response_bytes: null, max_request_ids: null, trace_max_flows: null, trace_max_hops: null, trace_max_output_bytes: null, compile_max_bytes: null, adr_max_rows: null, adr_max_output_bytes: null },
  evidence: { max_file_validation_bytes: null, max_span_bytes: null },
  scope: { max_sources: null, max_source_registry_bytes: null, max_event_registry_bytes: null, max_directory_entries: null, max_artifact_bytes: null, max_metadata_bytes: null, max_corpus_bytes: null },
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
        accounting limits, not a guarantee of process memory usage. The aggregate
        reservation must cover all connection payloads plus at least one worker.</p>
      <p>Acquisition mode is complete or scoped. Complete mode reads the whole recovered graph
        within the supplied limits and supports complete artifact compilation. Scoped mode provides
        bounded discovery handles, selected fact detail, and retained evidence with partial coverage;
        complete artifact compilation is unavailable. Scoped artifacts cover a whole connected data-entity component
        or one rule observation; a large component can exceed the supplied limits. Flows remain unavailable.
        The template uses complete mode to preserve
        the existing behavior.</p>
      <details><summary>Required policy fields</summary>
        <p>Replace every null with a positive integer. Times are milliseconds and sizes are bytes.
          Scope source and registry limits apply per observation; artifact and corpus bytes apply per repo.
          Directory and metadata limits apply per scan. Export mode is verified-only or best-effort.
          This template supplies no numeric defaults.</p>
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
      <button disabled={disabled || props.status !== 'running'}
        onClick={props.onRotate}>Rotate client credentials</button>
      <button disabled={disabled} onClick={props.onRefresh}>Refresh read service</button>
      <button disabled={disabled || props.status !== 'running'}
        onClick={props.onConfiguration}>Show client configuration</button>
      <button disabled={!props.available || props.skillBusy} onClick={props.onSkill}>
        {props.skillBusy ? 'Loading packaged client skill…' : 'Show packaged client skill'}
      </button>
    </div>
    {props.skill?.status === 'unavailable' && <p role="alert">The packaged client skill is unavailable or does not match this build. Reinstall a complete matching app bundle.</p>}
    {props.skill?.status === 'available' && <section className="mcp-client-skill" aria-label="Packaged client skill">
      <h4>Client skill · Cartograph {props.skill.app_version}</h4>
      <p>Skill location: <code>{props.skill.skill_path}</code></p>
      <p>Read SKILL.md first, then open references as needed. To install, use your client’s supported skill installation workflow with this complete folder.
        To remove it, use that client’s removal workflow. Installation and removal are separate requested actions;
        showing these files does not change client settings or enable read access.</p>
      {props.skill.files.map((file) => file.path === 'SKILL.md'
        ? <pre key={file.path} aria-label="Packaged SKILL.md">{file.content}</pre>
        : <details key={file.path}><summary>{file.path}</summary><pre>{file.content}</pre></details>)}
    </section>}
    <p>Rotating credentials disconnects existing clients. Old credentials stop working; request a new
      client configuration after rotation. Rotation does not restore investigation access.</p>
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
