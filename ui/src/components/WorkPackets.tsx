import { useEffect, useState } from 'react';
import { WorkPacketForm } from './WorkPacketForm';
import { packetFactKey, packetFactLabel, type PacketDetail, type PacketDispatch,
  type PacketJson } from '../workPacketTypes';
import type { useWorkPacketStore } from '../workPacketStore';

type PacketState = ReturnType<typeof useWorkPacketStore.getState>;
interface Props { state: PacketState }
const label = (key: string) => key.replaceAll('_', ' ').replace(/^./, (letter) => letter.toUpperCase());
const GROUPS = ['identity', 'intent', 'evidence', 'work', 'acceptance', 'handoff', 'return'] as const;

/** Source strings are rendered only as React text. No Markdown, HTML, or executable links. */
export function PacketValue({ value, protocol = true }: { value: PacketJson; protocol?: boolean }) {
  if (value === null) return <span className="muted">Not recorded</span>;
  if (typeof value !== 'object') return <span className="packet-source-text">{String(value)}</span>;
  if (Array.isArray(value)) return value.length ? <ol className="packet-values">{value.map((item, index) => <li key={index}><PacketValue value={item} protocol={protocol} /></li>)}</ol> :
    <span className="muted">None supplied; absence does not establish none exist.</span>;
  if (protocol && value.state === 'missing' && value.omission && typeof value.omission === 'object' && !Array.isArray(value.omission)) {
    return <div className="packet-omission"><strong>Unknown / missing</strong><p>{String(value.omission.detail)}</p>
      <small>{String(value.omission.class)} · {String(value.omission.seam)}</small></div>;
  }
  if (protocol && value.state === 'supplied') return <div><PacketValue value={value.value} />
    <details><summary>Recorded origin</summary><PacketValue value={value.origin} /></details></div>;
  return <>{protocol && value.provenance_problem && <p className="packet-omission"><strong>Invalid provenance · Gap</strong>: {String(value.provenance_problem)}</p>}<dl className="packet-fields">{Object.entries(value).map(([key, entry]) => <div key={key}><dt>{label(key)}</dt><dd><PacketValue value={entry} protocol={protocol && key !== 'props'} /></dd></div>)}</dl></>;
}

export function WorkPacketTray({ state, onOpen, showError = true }: Props & { onOpen: () => void; showError?: boolean }) {
  const count = state.facts.length + state.artifacts.length + state.decisions.length;
  if (!count && (!state.error || !showError)) return null;
  return <aside className="packet-tray" aria-label="Work packet selection">
    <div className="packet-heading"><strong>Agent work packet · {count} selected</strong>
      <button type="button" onClick={onOpen}>Prepare requested intent</button>
      <button type="button" disabled={state.busy} onClick={state.clearTray}>Clear selection</button></div>
    {state.snapshotId && <p>Recovered snapshot: <code>{state.snapshotId}</code></p>}
    <ul>{state.facts.map((fact) => <li key={packetFactKey(fact.reference)}><span>{packetFactLabel(fact.reference)} · <code>{fact.expected_digest}</code></span>
      <button type="button" disabled={state.busy} onClick={() => state.removeFact(packetFactKey(fact.reference))} aria-label={`Remove fact ${packetFactLabel(fact.reference)}`}>Remove</button></li>)}
    {state.artifacts.map((artifact) => <li key={`${artifact.id}:${artifact.mode}`}><span>{artifact.id} · {artifact.schema} · {artifact.mode} · <code>{artifact.content_id}</code></span>
      <button type="button" disabled={state.busy} onClick={() => state.removeArtifact(artifact.id, artifact.mode)} aria-label={`Remove artifact ${artifact.id}`}>Remove</button></li>)}
    {state.decisions.map((decision) => <li key={decision.content_hash}><span>Historical decision · <code>{decision.content_hash}</code> · record <code>{decision.expected_record_hash}</code></span>
      <button type="button" disabled={state.busy} onClick={() => state.removeDecision(decision.content_hash)} aria-label={`Remove decision ${decision.content_hash}`}>Remove</button></li>)}</ul>
    {showError && state.error && <p role="alert" className="error">{state.error}</p>}
  </aside>;
}

export function WorkPackets({ state, repositories }: Props & { repositories?: string[] }) {
  const [note, setNote] = useState('');
  const load = state.load;
  useEffect(() => { void load(); }, [load]);
  return <section className="work-packets card" aria-label="Local work packets">
    <header className="packet-heading"><div><h2>Agent work packets</h2>
      <p>Prepare an evidence-grounded request for an agent. Save and review the immutable body locally.</p></div>
      <button type="button" disabled={state.busy} onClick={() => void state.load()}>Refresh saved packets</button></header>
    <p className="muted">Requested intent remains a user claim. Curated context is unavailable. A packet runs only when a reviewed one is dispatched, and each packet shows its own execution state. Capability and egress requests confer no authority.</p>
    {state.error && <p role="alert" className="error">{state.error}</p>}
    {state.notice && <p role="status">{state.notice}</p>}
    <div className="packet-layout"><nav aria-label="Saved work packets"><h3>Saved packets</h3>
      {state.history.length ? <ul>{state.history.map((packet) => <li key={packet.packet_id}><button type="button" disabled={state.busy}
        onClick={() => void state.open(packet.packet_id)}><span>{packet.objective?.state === 'supplied' && typeof packet.objective.value === 'string' ? packet.objective.value : packet.packet_id}</span><small>{packet.review.status} · revision {packet.review.revision}</small></button></li>)}</ul> : <p className="muted">No saved packets in this page.</p>}
      {state.historyHasMore && <button type="button" disabled={state.busy} onClick={() => void state.load(true)}>Load more saved packets</button>}
      <p className="muted">Reopen reads the saved body and validates its identity.</p></nav>
      <div><details open={!state.detail}><summary>Requested intent and handoff form</summary>
        <WorkPacketForm state={state} repositories={repositories} />
      </details>
      {state.detail && <PacketDetailView detail={state.detail} />}
      {state.detail && <section className="packet-review" aria-label="Packet review"><h3>Review · revision {state.detail.review.revision}</h3>
        <p>Review changes the separate revisioned record. Evidence, source freshness, and execution remain independent.</p>
        <label>Review note<textarea value={note} maxLength={8192} rows={3} onChange={(event) => setNote(event.target.value)} /></label>
        <div className="packet-heading"><button type="button" disabled={state.busy} onClick={() => void state.review('reviewed', note)}>Mark reviewed</button>
          <button type="button" disabled={state.busy} onClick={() => void state.review('changes_requested', note)}>Request changes</button>
          <button type="button" disabled={state.busy} onClick={() => void state.assess()}>Assess source freshness</button>
          <button type="button" disabled={state.busy} onClick={() => void state.exportPacket()}>Export packet Markdown</button>
</div></section>}
      </div></div>
  </section>;
}

/** A dispatch's local branch, as recorded; nothing was pushed or merged. */
function PacketDispatchView({ dispatch }: { dispatch: PacketDispatch }) {
  const paths = dispatch.changed_paths.length + dispatch.changed_paths_omitted;
  return <section aria-label="Packet dispatch"><h4>Dispatched over ACP</h4>
    <p>Stop: <strong>{dispatch.stop}</strong> · at review revision {dispatch.reviewed_revision}</p>
    <p>Local branch <code>{dispatch.branch}</code> from <code>{dispatch.base_revision}</code>{dispatch.head_revision
      ? <> · commit <code data-testid="packet-dispatch-head">{dispatch.head_revision}</code></>
      : <> · no commit</>}</p>
    <p>{paths} changed {paths === 1 ? 'path' : 'paths'} · {dispatch.refused_requests} agent {dispatch.refused_requests === 1 ? 'request' : 'requests'} refused. Not pushed or merged; review the branch before merging it.</p>
    {dispatch.changed_paths.length > 0 && <ul>{dispatch.changed_paths.map((path) => <li key={path}><code>{path}</code></li>)}</ul>}
    {dispatch.changed_paths_omitted > 0 && <p className="muted">{dispatch.changed_paths_omitted} more not listed.</p>}
  </section>;
}

export function PacketDetailView({ detail }: { detail: PacketDetail }) {
  return <article className="packet-detail" aria-label="Saved packet detail"><h3>Saved immutable packet</h3>
    <p>Packet body schema: {detail.body.schema_version}</p>
    <p>Body identity: <code data-testid="packet-body-identity">{detail.packet_id}</code></p>
    <p>Review: <strong>{detail.review.status}</strong> · revision {detail.review.revision}</p>
    {detail.review.note && <p className="packet-source-text">Review note: {detail.review.note}</p>}
    <p>Source freshness: <strong>{detail.freshness.state}</strong> · checked {detail.freshness.checked_at ?? 'not yet assessed'}</p>
    <p>Execution: <strong data-testid="packet-execution">{detail.execution}</strong></p>
    {detail.dispatch && <PacketDispatchView dispatch={detail.dispatch} />}
    <details><summary>Lineage and independent freshness observations</summary><h4>Predecessors</h4>
      {detail.lineage.predecessor_ids.length ? <ul>{detail.lineage.predecessor_ids.map((id) => <li key={id}><code>{id}</code></li>)}</ul> : <p>None recorded.</p>}
      <PacketValue value={detail.freshness as unknown as PacketJson} /></details>
    {GROUPS.map((group) => <section key={group} aria-label={`Packet ${group}`}><h3>{label(group)}</h3><PacketValue value={detail.body[group]} /></section>)}
  </article>;
}
