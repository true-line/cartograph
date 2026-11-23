import { create } from 'zustand';
import { saveExport } from './saveExport';
import { invokeOr } from './tauri';
import type { FactKey } from './primarySourceStore';
import { packetFactKey, type GeneratePacketRequest, type PacketArtifactSelection, type PacketDecisionSelection,
  type PacketDetail, type PacketExport, type PacketFactSelection, type PacketSelectionSnapshot, type PacketSummary } from './workPacketTypes';

const message = (error: unknown): string => {
  const failure = typeof error === 'object' && error !== null ? error as { code?: string; message?: string; current_review?: { revision: number; status: string } } : null;
  const code = failure?.code ?? (typeof error === 'string' ? error : undefined);
  if (code === 'review_conflict') return `Review conflict: another review was saved${failure?.current_review ? ` at revision ${failure.current_review.revision} (${failure.current_review.status})` : ''}. The latest saved review was requested; inspect it before reviewing again.`;
  if (code === 'selection_conflict') return 'Selection conflict: selected evidence changed. Inspect the recovered evidence and select it again.';
  if (code === 'limit_exceeded') return 'Packet limit exceeded. Reduce selected evidence or requested content.';
  if (code === 'unavailable_evidence') return 'Selected evidence is unavailable. Inspect the selection and retained evidence.';
  if (code === 'storage_failure') return 'The local packet store could not complete this operation.';
  if (code === 'invalid_input') return 'Packet input is invalid. Check required supplied values and evidence references.';
  if (code === 'not_reviewed') return 'Only a reviewed packet can be dispatched. Mark it reviewed first.';
  if (code === 'unpinned_revision') return 'This packet does not name exactly one source pinned to a commit, so it cannot be dispatched.';
  if (code === 'agent_not_configured') return 'No ACP agent is configured. Set one under Configure → ACP agent, then dispatch again.';
  if (code === 'agent_unavailable') return 'The configured ACP agent could not be started. Check its program path under Configure → ACP agent.';
  if (code === 'already_dispatched') return 'This packet already has a dispatch worktree. Review or remove its branch before dispatching again.';
  if (code === 'dispatch_failed') return 'The dispatch could not prepare its worktree. Nothing was recorded.';
  return failure?.message ?? (typeof error === 'string' ? error : JSON.stringify(error));
};
interface WorkPacketState {
  snapshotId: string | null; facts: PacketFactSelection[]; artifacts: PacketArtifactSelection[];
  decisions: PacketDecisionSelection[]; history: PacketSummary[]; historyHasMore: boolean; detail: PacketDetail | null;
  busy: boolean; error: string | null; notice: string | null;
  addFact: (reference: FactKey) => Promise<void>;
  addArtifact: (artifact: PacketArtifactSelection) => Promise<void>;
  addDecision: (contentHash: string) => Promise<void>;
  removeFact: (key: string) => void; removeArtifact: (id: string, mode: string) => void;
  removeDecision: (hash: string) => void; clearTray: () => void;
  load: (append?: boolean) => Promise<void>; open: (packetId: string) => Promise<void>;
  generate: (request: GeneratePacketRequest) => Promise<void>;
  review: (decision: 'reviewed' | 'changes_requested', note: string) => Promise<void>;
  assess: () => Promise<void>; exportPacket: () => Promise<void>;
  dispatch: () => Promise<void>;
}
export const useWorkPacketStore = create<WorkPacketState>((set, get) => {
  const run = async (operation: () => Promise<void>) => {
    if (get().busy) return;
    set({ busy: true, error: null, notice: null });
    try { await operation(); } catch (error) { set({ error: message(error) }); }
    finally { set({ busy: false }); }
  };
  const selection = async (facts: FactKey[], decisions: string[]) => {
    const result = await invokeOr<PacketSelectionSnapshot | null>('prepare_work_packet_selection', null,
      { facts, interpretationDecisions: decisions });
    if (!result) throw new Error('Authoritative packet selection is unavailable. Connect to the local core.');
    const state = get();
    if (state.snapshotId && state.snapshotId !== result.snapshot_id) {
      throw new Error('Selection conflict: recovered snapshot changed. Clear the tray and select evidence again.');
    }
    if (state.facts.some((existing) => !result.facts.some((item) => packetFactKey(item.reference) === packetFactKey(existing.reference) && item.expected_digest === existing.expected_digest)) ||
      state.decisions.some((existing) => !result.interpretation_decisions.some((item) => item.content_hash === existing.content_hash && item.expected_record_hash === existing.expected_record_hash))) {
      throw new Error('Selection conflict: selected fact or historical decision changed. Remove the selection and inspect it again.');
    }
    set({ snapshotId: result.snapshot_id, facts: result.facts, decisions: result.interpretation_decisions });
  };
  return {
    snapshotId: null, facts: [], artifacts: [], decisions: [], history: [], historyHasMore: false, detail: null,
    busy: false, error: null, notice: null,
    addFact: (reference) => run(async () => {
      if (get().facts.length >= 64 && !get().facts.some((fact) => packetFactKey(fact.reference) === packetFactKey(reference))) throw new Error('A packet accepts at most 64 facts.');
      const references = get().facts.map((fact) => fact.reference);
      if (!references.some((item) => packetFactKey(item) === packetFactKey(reference))) references.push(reference);
      await selection(references, get().decisions.map((item) => item.content_hash));
    }),
    addArtifact: (artifact) => run(async () => {
      if (!get().snapshotId) await selection(get().facts.map((fact) => fact.reference), get().decisions.map((item) => item.content_hash));
      const existing = get().artifacts.find((item) => item.id === artifact.id && item.mode === artifact.mode);
      if (existing && (existing.content_id !== artifact.content_id || existing.schema !== artifact.schema)) throw new Error('Selection conflict: artifact identity changed. Remove the previous selection first.');
      if (!existing && get().artifacts.length >= 4) throw new Error('A packet accepts at most four artifacts.');
      if (!existing) set({ artifacts: [...get().artifacts, artifact] });
    }),
    addDecision: (hash) => run(async () => {
      const hashes = [...new Set([...get().decisions.map((item) => item.content_hash), hash])];
      if (hashes.length > 64) throw new Error('A packet accepts at most 64 historical decisions.');
      await selection(get().facts.map((fact) => fact.reference), hashes);
    }),
    removeFact: (key) => {
      const facts = get().facts.filter((fact) => packetFactKey(fact.reference) !== key);
      set({ facts, ...(!facts.length && !get().artifacts.length && !get().decisions.length ? { snapshotId: null } : {}) });
    },
    removeArtifact: (id, mode) => {
      const artifacts = get().artifacts.filter((item) => item.id !== id || item.mode !== mode);
      set({ artifacts, ...(!artifacts.length && !get().facts.length && !get().decisions.length ? { snapshotId: null } : {}) });
    },
    removeDecision: (hash) => {
      const decisions = get().decisions.filter((item) => item.content_hash !== hash);
      set({ decisions, ...(!decisions.length && !get().facts.length && !get().artifacts.length ? { snapshotId: null } : {}) });
    },
    clearTray: () => set({ snapshotId: null, facts: [], artifacts: [], decisions: [], error: null }),
    load: (append = false) => run(async () => {
      const previous = append ? get().history : [];
      const page = await invokeOr<PacketSummary[]>('list_work_packets', [], { limit: 10, after: previous.at(-1)?.packet_id ?? null });
      set({ history: [...previous, ...page], historyHasMore: page.length === 10 });
    }),
    open: (packetId) => run(async () => {
      const detail = await invokeOr<PacketDetail | null>('get_work_packet', null, { packetId });
      if (!detail) throw new Error('Saved packet unavailable.');
      set({ detail });
    }),
    generate: (request) => run(async () => {
      const detail = await invokeOr<PacketDetail | null>('generate_work_packet', null, { requestJson: JSON.stringify(request) });
      if (!detail) throw new Error('Packet generation requires the local core.');
      set({ detail, notice: 'Packet saved locally. Requested capabilities grant no authority.' });
    }),
    review: (decision, note) => run(async () => {
      const detail = get().detail;
      if (!detail) return;
      try {
        const reviewed = await invokeOr<PacketDetail | null>('review_work_packet', null,
          { request: { packet_id: detail.packet_id, expected_revision: detail.review.revision, decision, ...(note.trim() ? { note } : {}) } });
        if (!reviewed) throw new Error('Review unavailable.');
        set({ detail: reviewed });
      } catch (error) {
        // Keep the saved body and refresh the winning revision after a CAS conflict.
        try {
          const latest = await invokeOr<PacketDetail | null>('get_work_packet', null, { packetId: detail.packet_id });
          if (latest) set({ detail: latest });
        } catch { /* Preserve the explicit review failure when the reload is also unavailable. */ }
        throw error;
      }
    }),
    assess: () => run(async () => {
      const detail = get().detail; if (!detail) return;
      const assessed = await invokeOr<PacketDetail | null>('assess_work_packet', null, { packetId: detail.packet_id });
      if (!assessed) throw new Error('Freshness assessment unavailable.');
      set({ detail: assessed });
    }),
    dispatch: () => run(async () => {
      const detail = get().detail;
      if (!detail) return;
      set({ notice: 'Dispatching to the configured ACP agent. This runs until its turn ends.' });
      const dispatched = await invokeOr<PacketDetail | null>('dispatch_work_packet', null, { packetId: detail.packet_id });
      if (!dispatched || dispatched.packet_id !== detail.packet_id) throw new Error('Dispatch requires the local core.');
      set({ detail: dispatched, notice: dispatched.dispatch?.head_revision
        ? `Dispatched. The agent's work is on local branch ${dispatched.dispatch.branch}; review it before merging.`
        : 'Dispatched. The agent made no commit.' });
    }),
    exportPacket: () => {
      const priorNotice = get().notice;
      return run(async () => {
        const detail = get().detail;
        if (!detail) {
          set({ notice: priorNotice });
          return;
        }
        const exported = await invokeOr<PacketExport | null>('export_work_packet', null, { packetId: detail.packet_id });
        if (!exported || exported.packet_id !== detail.packet_id) throw new Error('Export identity did not match the saved packet.');
        const outcome = await saveExport(exported.file_name, exported.content);
        set({ notice: outcome === 'saved' ? 'Packet exported.' : priorNotice });
      });
    },
  };
});
