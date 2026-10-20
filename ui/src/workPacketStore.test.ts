import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useWorkPacketStore } from './workPacketStore';
import type { PacketSelectionSnapshot } from './workPacketTypes';

beforeEach(() => { vi.stubGlobal('window', {}); useWorkPacketStore.setState(useWorkPacketStore.getInitialState(), true); });
afterEach(() => { clearMocks(); vi.unstubAllGlobals(); useWorkPacketStore.setState(useWorkPacketStore.getInitialState(), true); });
const node = { kind: 'node' as const, id: 'rule:stock' };
const edge = { kind: 'edge' as const, source: node.id, label: 'GOVERNS', destination: 'endpoint:reserve' };
const snapshot: PacketSelectionSnapshot = { snapshot_id: 'context-v1:original', facts: [{ reference: node, expected_digest: 'host-node-digest' }], interpretation_decisions: [] };

describe('host-authoritative packet tray (AC-0248, AC-0251)', () => {
  it('sends typed references only and retains the host digests; no browser provenance is submitted', async () => {
    const requests: unknown[] = [];
    mockIPC((command, args) => {
      expect(command).toBe('prepare_work_packet_selection'); requests.push(args);
      return requests.length === 1 ? snapshot : { ...snapshot, facts: [...snapshot.facts, { reference: edge, expected_digest: 'host-edge-digest' }] };
    });
    await useWorkPacketStore.getState().addFact(node);
    await useWorkPacketStore.getState().addFact(edge);
    expect(requests).toEqual([{ facts: [node], interpretationDecisions: [] }, { facts: [node, edge], interpretationDecisions: [] }]);
    expect(useWorkPacketStore.getState().facts.map((fact) => fact.expected_digest)).toEqual(['host-node-digest', 'host-edge-digest']);
  });
  it('refuses to rebind an existing selection to a newer snapshot', async () => {
    let count = 0;
    mockIPC(() => ++count === 1 ? snapshot : { ...snapshot, snapshot_id: 'context-v1:changed', facts: [...snapshot.facts, { reference: edge, expected_digest: 'new-edge' }] });
    await useWorkPacketStore.getState().addFact(node);
    await useWorkPacketStore.getState().addFact(edge);
    expect(useWorkPacketStore.getState().snapshotId).toBe(snapshot.snapshot_id);
    expect(useWorkPacketStore.getState().facts).toEqual(snapshot.facts);
    expect(useWorkPacketStore.getState().error).toMatch(/Selection conflict/);
  });
  it('refuses changed complete historical decision hashes even when graph identity is equal', async () => {
    let count = 0;
    mockIPC(() => ({ ...snapshot, interpretation_decisions: [{ content_hash: 'a'.repeat(64), expected_record_hash: ++count === 1 ? 'original-record' : 'changed-record' }] }));
    await useWorkPacketStore.getState().addDecision('a'.repeat(64));
    await useWorkPacketStore.getState().addFact(node);
    expect(useWorkPacketStore.getState().decisions[0].expected_record_hash).toBe('original-record');
    expect(useWorkPacketStore.getState().error).toMatch(/historical decision changed/);
  });
  it('preserves full artifact schema/content/mode and rejects a changed preview selection', async () => {
    mockIPC(() => ({ ...snapshot, facts: [] }));
    const artifact = { id: 'rules', schema: 'spec-artifact-v1', content_id: 'spec-v1:old', mode: 'best-effort' as const };
    await useWorkPacketStore.getState().addArtifact(artifact);
    await useWorkPacketStore.getState().addArtifact({ ...artifact, content_id: 'spec-v1:new' });
    expect(useWorkPacketStore.getState().artifacts).toEqual([artifact]);
    expect(useWorkPacketStore.getState().error).toMatch(/artifact identity changed/);
    useWorkPacketStore.getState().removeArtifact(artifact.id, artifact.mode);
    expect(useWorkPacketStore.getState().snapshotId).toBeNull();
  });
});
