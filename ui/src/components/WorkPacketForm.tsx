import { useState, type ReactNode } from 'react';
import { missingPacketField, type GeneratePacketRequest, type PacketJson, type PacketKnown } from '../workPacketTypes';
import type { useWorkPacketStore } from '../workPacketStore';

type State = ReturnType<typeof useWorkPacketStore.getState>;
const title = (key: string) => key.replaceAll('_', ' ').replace(/^./, (letter) => letter.toUpperCase());
const fields = {
  intent: ['objective', 'expected_outcome', 'current_behavior', 'intended_behavior', 'scope', 'non_goals', 'business_rationale'],
  work: ['affected_components', 'proposed_approach', 'dependencies', 'open_decisions'],
  acceptance: ['positive_cases', 'negative_cases', 'proposed_verification'],
  handoff: ['requested_capabilities', 'checkout_boundaries', 'tool_and_egress_requests', 'budgets', 'proposed_role', 'authorization_references'],
  return: ['implementation_revision', 'pull_request', 'execution_receipts', 'source_configuration_changes', 'reingest_comparison', 'residual_gaps'],
} as const;
const listFields = new Set(['proposed_approach', 'open_decisions', 'requested_capabilities', 'checkout_boundaries', 'tool_and_egress_requests', 'authorization_references', 'execution_receipts', 'source_configuration_changes', 'residual_gaps']);
const lines = (value: string) => value.split('\n').map((line) => line.trim()).filter(Boolean);
const hasContent = (value: PacketJson | undefined): boolean => value !== undefined && value !== null &&
  (typeof value === 'string' ? value.trim().length > 0 : Array.isArray(value) ? value.length > 0 && value.every(hasContent) :
    typeof value === 'object' ? Object.values(value).some(hasContent) : true);
const text = (value: PacketJson | undefined) => typeof value === 'string' ? value : '';
const object = (value: PacketJson): Record<string, PacketJson> => typeof value === 'object' && value !== null && !Array.isArray(value) ? value : {};

function KnowledgeField({ name, supplied, onToggle, children }: { name: string; supplied: boolean; onToggle: (supplied: boolean) => void; children: ReactNode }) {
  return <fieldset className="packet-request-field"><legend>{title(name)} · requested</legend>
    <label className="packet-unknown"><input type="checkbox" checked={!supplied} onChange={(event) => onToggle(!event.target.checked)} />Unknown / not supplied: {title(name).toLowerCase()}</label>
    {supplied ? children : <p className="muted">Recorded as missing_input / interpretation with a field-specific omission.</p>}
  </fieldset>;
}

/** Requested-only editors build typed values; no browser input supplies evidence or review authority. */
export function WorkPacketForm({ state, repositories = [] }: { state: State; repositories?: string[] }) {
  const [values, setValues] = useState<Record<string, PacketJson>>({});
  const [requestedBy, setRequestedBy] = useState('');
  const [actor, setActor] = useState('');
  const [journey, setJourney] = useState('');
  const [supersedes, setSupersedes] = useState(false);
  const write = (name: string, value: PacketJson) => setValues((previous) => ({ ...previous, [name]: value }));
  const array = (name: string): PacketJson[] => Array.isArray(values[name]) ? values[name] as PacketJson[] : [];
  const updateRow = (name: string, index: number, property: string, value: PacketJson) => write(name, array(name).map((item, row) => row === index ? { ...object(item), [property]: value } : item));
  const initial = (name: string): PacketJson => name === 'affected_components' || name === 'dependencies' || name === 'budgets' || ['positive_cases', 'negative_cases', 'proposed_verification'].includes(name) ? [] : '';
  const known = (name: string, group: string): PacketKnown => {
    const value = values[name];
    if (!hasContent(value)) return missingPacketField(`${group}.${name}`);
    const normalized = Array.isArray(value) ? value.map((item) => {
      const row = object(item);
      return ['positive_cases', 'negative_cases', 'proposed_verification'].includes(name) ? {
        ...row, steps: Array.isArray(row.steps) ? row.steps.filter((step) => typeof step === 'string' && step.trim()) : [],
        expected_evidence: Array.isArray(row.expected_evidence) ? row.expected_evidence.filter((entry) => typeof entry === 'string' && entry.trim()) : [],
      } : item;
    }) : value;
    return { state: 'supplied', value: listFields.has(name) ? lines(text(value)) : normalized,
      origin: { kind: 'requested_intent' } };
  };
  const renderRows = (name: string, template: Record<string, PacketJson>, render: (row: Record<string, PacketJson>, index: number) => ReactNode) => <>
    {array(name).map((row, index) => <div key={index} className="packet-request-field">{render(object(row), index)}
      <button type="button" onClick={() => write(name, array(name).filter((_, rowIndex) => rowIndex !== index))}>Remove {title(name).toLowerCase()} item {index + 1}</button></div>)}
    <button type="button" disabled={array(name).length >= 64} onClick={() => write(name, [...array(name), template])}>Add {title(name).toLowerCase()} item</button></>;
  const render = (name: string) => {
    if (name === 'affected_components') return renderRows(name, { repo_key: repositories[0] ?? '', path: null, fact_references: [] }, (row, index) => <>
      <label>Registered repository key<input required value={text(row.repo_key)} list="packet-repositories" maxLength={4096} onChange={(event) => updateRow(name, index, 'repo_key', event.target.value)} /></label>
      <label>Optional repository path<input value={text(row.path)} maxLength={4096} onChange={(event) => updateRow(name, index, 'path', event.target.value.trim() || null)} /></label>
      <p className="muted">Repository and path describe requested scope. They grant no filesystem authority. Fact references: none supplied.</p></>);
    if (name === 'dependencies') return renderRows(name, { description: '', evidence_ids: [] }, (row, index) => <label>Dependency description<textarea required value={text(row.description)} maxLength={4096} onChange={(event) => updateRow(name, index, 'description', event.target.value)} /></label>);
    if (name === 'budgets') return renderRows(name, { resource: '', limit: '' }, (row, index) => <>
      <label>Requested resource<input required value={text(row.resource)} maxLength={4096} onChange={(event) => updateRow(name, index, 'resource', event.target.value)} /></label>
      <label>Requested limit<input required value={text(row.limit)} maxLength={4096} onChange={(event) => updateRow(name, index, 'limit', event.target.value)} /></label></>);
    if (['positive_cases', 'negative_cases', 'proposed_verification'].includes(name)) return renderRows(name,
      { id: `${name}-${array(name).length + 1}`, description: '', steps: [], expected_evidence: [], evidence_ids: [] }, (row, index) => <>
        <label>Local procedure ID<input required value={text(row.id)} maxLength={4096} onChange={(event) => updateRow(name, index, 'id', event.target.value)} /></label>
        <label>Requested {name === 'proposed_verification' ? 'verification procedure' : 'case'} description<textarea required value={text(row.description)} maxLength={4096} onChange={(event) => updateRow(name, index, 'description', event.target.value)} /></label>
        <label>Ordered steps (one per line)<textarea value={Array.isArray(row.steps) ? row.steps.join('\n') : ''} maxLength={4096} onChange={(event) => updateRow(name, index, 'steps', event.target.value.split('\n'))} /></label>
        <label>Expected evidence (one per line)<textarea value={Array.isArray(row.expected_evidence) ? row.expected_evidence.join('\n') : ''} maxLength={4096} onChange={(event) => updateRow(name, index, 'expected_evidence', event.target.value.split('\n'))} /></label>
        <p className="muted">This requested procedure is not a discovered test, execution receipt, coverage result, or assertion support.</p></>);
    return <label>Requested {title(name).toLowerCase()}<textarea value={text(values[name])} rows={2} maxLength={4096}
      placeholder={listFields.has(name) ? 'One requested item per line' : 'Describe the request; this is not recovered confirmation'} onChange={(event) => write(name, event.target.value)} /></label>;
  };
  const generate = () => {
    if (!state.snapshotId) return;
    const group = <K extends keyof typeof fields>(name: K) => Object.fromEntries(fields[name].map((key) => [key, known(key, name)])) as unknown as GeneratePacketRequest[K];
    const request: GeneratePacketRequest = { expected_snapshot_id: state.snapshotId, facts: state.facts, artifacts: state.artifacts,
      interpretation_decisions: state.decisions,
      intent: { ...group('intent'), ...(requestedBy.trim() ? { requested_by: requestedBy.trim() } : {}),
        ...(actor.trim() ? { requested_actor: actor.trim() } : {}), ...(journey.trim() ? { requested_journey: journey.trim() } : {}) },
      work: group('work'), acceptance: { ...group('acceptance'), discovered_tests: [], structural_test_associations: [], execution_evidence: [], coverage_evidence: [], assertion_support: [] },
      handoff: group('handoff'), return: group('return'), ...(supersedes && state.detail ? { supersedes: [state.detail.packet_id] } : {}) };
    void state.generate(request);
  };
  return <form aria-label="Requested work packet intent" onSubmit={(event) => { event.preventDefault(); generate(); }}>
    <p>Select recovered facts in Atlas or artifacts and historical decisions in Spec Workbench. The tray binds exact host identities.</p>
    <p className="muted">A blank supplied field is retained as an explicit omission. Each narrative is limited to 4 KiB by the local core.</p>
    <datalist id="packet-repositories">{repositories.map((repo) => <option key={repo}>{repo}</option>)}</datalist>
    <fieldset disabled={state.busy}>
      <label>Requested by (optional label)<input maxLength={4096} value={requestedBy} onChange={(event) => setRequestedBy(event.target.value)} /></label>
      <p className="muted">This label does not assert authenticated identity.</p>
      <details><summary>Optional explicit actor and journey</summary>
        <label>Requested actor<input value={actor} maxLength={4096} onChange={(event) => setActor(event.target.value)} /></label>
        <label>Requested journey<input value={journey} maxLength={4096} onChange={(event) => setJourney(event.target.value)} /></label></details>
      {Object.entries(fields).map(([group, names]) => <details key={group} open={group === 'intent'}><summary>{title(group)}</summary>
        {names.map((name) => <KnowledgeField key={name} name={name} supplied={values[name] !== undefined} onToggle={(supplied) => {
          if (supplied) write(name, initial(name)); else setValues((previous) => { const next = { ...previous }; delete next[name]; return next; });
        }}>{render(name)}</KnowledgeField>)}</details>)}
      {state.detail && <label><input type="checkbox" checked={supersedes} onChange={(event) => setSupersedes(event.target.checked)} />This new body supersedes the open packet</label>}
      <button type="submit" disabled={!state.snapshotId || state.facts.length + state.artifacts.length === 0}>Generate and open saved packet</button>
    </fieldset>
  </form>;
}
