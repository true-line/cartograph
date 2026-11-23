import { admissionCatalog, completionChoices } from './completionFixtures';
import type { Meta, StoryObj } from '@storybook/react-vite';
import { sectionDocumentFixture } from './sectionDocumentFixtures';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, fireEvent, userEvent, waitFor, within } from 'storybook/test';
import App from './App';
import { useAgentChatDockStore } from './agentChatDockStore';
import { useInvestigationStore } from './investigationStore';
import { inspectionCatalog, inspectionDetail, inspectionTarget, investigationCatalog, investigationCitations, investigationConsent, investigationDetail, investigationEvents, investigationResult } from './investigationFixtures';
import type { InvestigationChat, InvestigationDetail, InvestigationEvent, InvestigationInspectionTarget, StartInvestigationRequest } from './investigationTypes';
import { useSavedResultNoticeStore } from './savedResultNoticeStore';
import { CURATED_GRAPH_BASIS, NO_COUNTS, UNVERIFIED_RECORD } from './curatedFixtures';
import { usePrimarySourceStore, type CapturedDescription, type FactKey } from './primarySourceStore';
import {
  useAppStore,
  type AssertionDecisionRecord,
  type ComputedCuratedRead,
  type Flow,
  type Provenance,
  type SpecAssertion,
  type SpecBundle,
  type SpecViewBundle,
  type StagedProposal,
} from './store';

/**
 * Full-app story against a fake Rust core: `mockIPC` installs a fake
 * `__TAURI_INTERNALS__`, so `inTauri()` is true and every command resolves
 * from the handler below — no backend involved. This is the pattern for any
 * story that needs command data.
 */
interface MockJob {
  id: number;
  kind: string;
  status: string;
  execution_tracking: 'recorded' | 'legacy_unknown';
  investigation_id?: string;
  created_at: string;
  updated_at: string;
}

// Non-ASCII before the span keeps this honest: provenance spans are UTF-8
// byte offsets, so compute them with TextEncoder, never UTF-16 indexOf.
const FAKE_SOURCE = `// naïve café route — voilà 🚀
import express from 'express';
const app = express();
app.get('/users', listUsers);
`;
const byteLen = (s: string) => new TextEncoder().encode(s).length;
const SPAN_TEXT = "app.get('/users', listUsers)";
const SPAN_START = byteLen(FAKE_SOURCE.slice(0, FAKE_SOURCE.indexOf(SPAN_TEXT)));
const SPAN_END = SPAN_START + byteLen(SPAN_TEXT);

const FAKE_PROVENANCE: Provenance = {
  tier: 'Deterministic',
  confidence_tier: 'Confirmed',
  evidence: [
    {
      repo: 'local',
      path: 'src/app.ts',
      byte_start: SPAN_START,
      byte_end: SPAN_END,
      commit_sha: 'workdir',
    },
  ],
  extractor_id: 't0.adapter-ts',
  content_hash: 'a'.repeat(64),
};

const FAKE_ENDPOINT = {
  id: 'ep:GET:/users',
  label: 'Endpoint',
  props: {
    method: 'GET',
    path: '/users',
    prov: FAKE_PROVENANCE,
  },
};

const FAKE_REPO = {
  // A tempting historical root from a different repo must never resolve evidence.
  id: 'repo:unrelated',
  label: 'Repo',
  props: { root: '/fake/repo' },
};

const FAKE_INFERRED: SpecAssertion = {
  id: 'node:adr:async-orders',
  subject_id: 'adr:async-orders',
  subject_kind: 'ADR',
  summary: 'ADR: asynchronous order fulfillment',
  provenance: {
    tier: 'Semantic',
    confidence_tier: 'InferredStrong',
    evidence: [{
      repo: 'local',
      path: 'src/orders.ts',
      byte_start: 20,
      byte_end: 64,
      commit_sha: 'workdir',
    }],
    extractor_id: 't2.semantic',
    content_hash: 'b'.repeat(64),
  },
};

const FAKE_SPEC: SpecBundle = {
  mode: 'best-effort',
  artifacts: [
    'user_stories.md',
    'US-TM.md',
    'flow_dossiers.md',
    'topology.md',
    'data_model.md',
    'adrs.md',
    'gap_register.md',
    'drift_register.md',
    'security.md',
  ].map((fileName, index) => {
    const content = `# ${fileName}\n\n## Assertions and inline provenance\n`;
    const assertions = index === 0 ? [{
        id: 'node:ep:GET:/users',
        subject_id: FAKE_ENDPOINT.id,
        subject_kind: 'Endpoint',
        summary: 'Endpoint: GET /users',
        provenance: FAKE_PROVENANCE,
      }] : index === 5 ? [FAKE_INFERRED] : index === 6 ? [{
        // An edge gap: no atlas node carries this id, so the register row
        // must open evidence from the assertion's own provenance (#137).
        id: 'edge:sym:capture CALLS sym:sync',
        subject_id: 'sym:capture CALLS sym:sync',
        subject_kind: 'CALLS',
        summary: 'sym:capture CALLS sym:sync — callee not statically resolvable',
        provenance: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' as const },
      }, {
        // A gap node: its row opens the Resolution Strategy modal (#120).
        id: 'node:gap:sync',
        subject_id: 'gap:sync',
        subject_kind: 'Gap',
        summary: 'Gap: remote sync target computed at runtime',
        provenance: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' as const },
      }] : [];
    const id =
      fileName === 'flow_dossiers.md'
        ? 'flow-dossiers'
        : fileName === 'topology.md'
          ? 'topology'
          : fileName === 'data_model.md'
            ? 'data-model'
            : `artifact-${index}`;
    return {
      id,
      file_name: fileName,
      title: index === 0 ? 'User stories' : index === 5 ? 'Architecture decisions' : fileName,
      format: fileName.endsWith('.mmd') ? 'mermaid' : 'markdown',
      content,
      schema: `cartograph.spec/${fileName}@1`,
      content_id: `spec-artifact-v1:fixture-${fileName}`,
      content_truncated: false,
      content_byte_len: content.length,
      assertions,
      assertions_truncated: false,
      assertions_total: assertions.length,
    };
  }),
  assertion_count: 2,
  gap_count: 0,
  drift_count: 0,
  security_count: 0,
};

const FAKE_FLOW: Flow = {
  trigger: 'ep:GET:/users',
  trigger_kind: 'Endpoint',
  trigger_name: 'GET /users',
  hops: [
    {
      label: 'HANDLES',
      src: 'ep:GET:/users',
      dst: 'sym:app.ts#listUsers',
      src_name: 'GET /users',
      dst_name: 'listUsers',
      tier: 'Deterministic',
      confidence: 'Confirmed',
      evidence: 'src/app.ts bytes 92..120',
      provenance: FAKE_PROVENANCE,
      gap_reason: null,
      attempted_tiers: [],
    },
    {
      label: 'CALLS',
      src: 'sym:app.ts#listUsers',
      dst: 'gap:sync',
      src_name: 'listUsers',
      dst_name: 'GAP: remote sync target computed at runtime',
      tier: 'Deterministic',
      confidence: 'Gap',
      evidence: 'src/app.ts bytes 92..120',
      provenance: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' },
      gap_reason: 'remote sync target computed at runtime',
      attempted_tiers: ['T0', 'T1'],
    },
  ],
  status: 'Partial',
  score: 0.5,
  depth_limited: false,
};

function fakeSpecViews(mode: SpecBundle['mode']): SpecViewBundle {
  return {
    mode,
    topology: {
      artifact_id: 'topology',
      schema: 'cartograph.spec/topology.md@1',
      content_id: 'spec-artifact-v1:fixture-topology.md',
      mode,
      nodes: [
        {
          id: 'res:aws_sqs_queue.orders',
          label: 'Resource',
          props: { name: 'aws_sqs_queue.orders', prov: FAKE_PROVENANCE },
        },
      ],
      nodes_total: 1,
      edges: [],
      edges_total: 0,
      flows_total: 0,
      deployments: [],
      truncated: false,
    },
    flows: {
      artifact_id: 'flow-dossiers',
      schema: 'cartograph.spec/flow_dossiers.md@1',
      content_id: 'spec-artifact-v1:fixture-flow_dossiers.md',
      mode,
      nodes: [
        FAKE_ENDPOINT,
        {
          id: 'sym:app.ts#listUsers',
          label: 'Symbol',
          props: { name: 'listUsers', prov: FAKE_PROVENANCE },
        },
        {
          id: 'gap:sync',
          label: 'Gap',
          props: {
            name: 'GAP: remote sync target computed at runtime',
            reason: 'remote sync target computed at runtime',
            prov: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' },
          },
        },
      ],
      nodes_total: 3,
      edges: [
        {
          src: 'ep:GET:/users',
          dst: 'sym:app.ts#listUsers',
          label: 'HANDLES',
          props: { prov: FAKE_PROVENANCE },
        },
        {
          src: 'sym:app.ts#listUsers',
          dst: 'gap:sync',
          label: 'CALLS',
          props: {
            gap_reason: 'remote sync target computed at runtime',
            prov: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' },
          },
        },
      ],
      edges_total: 2,
      flows: [FAKE_FLOW],
      flows_total: 1,
      deployments: [],
      truncated: false,
    },
    data_model: {
      artifact_id: 'data-model',
      schema: 'cartograph.spec/data_model.md@1',
      content_id: 'spec-artifact-v1:fixture-data_model.md',
      mode,
      nodes: [],
      nodes_total: 0,
      edges: [],
      edges_total: 0,
      flows_total: 0,
      truncated: false,
    },
  };
}

interface MockTier {
  tier: string;
  enabled: boolean;
  provider: string;
  consented: boolean;
  consent_disclosure: string | null;
  consented_at: string | null;
}

function stagedFixture(proposalId = 'proposal:host-result', decision: 'accepted' | 'rejected' | null = null): StagedProposal {
  return {
    proposal_id: proposalId,
    review_revision: decision ? 1 : 0,
    review_decision: decision,
    review_note: null,
    evidence_binding: 'working_tree_unverified',
    context_status: 'awaiting_reconciliation',
    created_at: '2026-09-10T12:00:00Z',
    reviewed_at: decision ? '2026-09-10T13:00:00Z' : null,
    gap_id: 'gap:sync',
    source_id: 'sym:capture',
    target_id: FAKE_ENDPOINT.id,
    edge_label: 'CALLS',
    annotation: `Saved annotation for ${proposalId}.`,
    basis_hash: 'b'.repeat(64),
    provenance: {
      ...FAKE_PROVENANCE,
      tier: 'Agentic', confidence_tier: 'InferredWeak', extractor_id: 't3.agent',
    },
  };
}

let reviewRequests: Record<string, unknown>[] = [];
let historyRequests: { limit: number; cursor: string | null }[] = [];
let evidenceRequests: Record<string, unknown>[] = [];
let capturedDescriptionRequests: Record<string, unknown>[] = [];
let repoRootLookups = 0;
let investigationRequests: { command: string; args: unknown }[] = [];
let releaseSavedNoticeChatRead: (() => void) | null = null;

function savedNoticeChat(id: string, question: string, includeResult = true): InvestigationChat {
  const specialist = investigationCatalog.specialists[0];
  const messages: InvestigationChat['messages'] = [
    { id: `question:${id}`, ordinal: 0, author: 'user', message: { kind: 'user_question', question } },
  ];
  if (includeResult) messages.push({ id: `result:${id}`, ordinal: 1, author: 'specialist', message: { kind: 'result_reference',
    task_id: id, result_id: `result:${id}`, result_hash: 'c'.repeat(64) } });
  return { chat_id: `saved-chat:${id}`, task_id: id,
    producer: { specialist_id: specialist.id, specialist_version: specialist.version,
      prompt_fingerprint: specialist.prompt_fingerprint, tier: specialist.tier,
      confidence_tier: specialist.confidence_tier, provider_mode: 'local', provider_id: 'ollama',
      model: 'fixture-local-model', protocol_version: null },
    messages };
}

let localHealthResponse: (() => unknown) | undefined;
function installFakeCore(options: {
  evidenceLessGraph?: boolean;
  staged?: StagedProposal[];
  pageSize?: number;
  reviewFailure?: 'stale' | 'null';
  reviewGate?: Promise<void>;
  unstagedResult?: boolean;
  historyGate?: Promise<void>;
  runGate?: Promise<void>;
  unavailableEvidence?: boolean;
  evidenceGate?: Promise<void>;
  capturedDescriptions?: boolean;
  inspectionSelection?: boolean;
  admissionSelection?: boolean;
  existingInvestigation?: InvestigationDetail;
  existingInvestigationEvents?: InvestigationEvent[];
  rejectJobActionOnce?: boolean;
  legacyRunningJob?: boolean;
  investigationFlow?: boolean;
  startUncertainOnce?: boolean;
  /** The first result read after completion races the commit and sees none. */
  straddleResultCommit?: boolean;
  savedNoticeChat?: InvestigationChat;
  deferSavedNoticeChatRead?: boolean;
  deferSavedNoticeChatAfterReads?: number;
  additionalSavedParent?: InvestigationDetail;
  additionalSavedExchanges?: InvestigationDetail[];
  savedNoticeChatFirstReadPartial?: boolean;
} = {}) {
  let staged = [...(options.staged ?? [])];
  // The curated read of the fake stores (AC-0655). Every staged record here
  // is a version 1 record, so an accepted one is listed as `unverified`, as
  // the host lists it. The revision changes with every review, as the
  // host's does, and the bundle's curated basis is taken from the same read.
  const fakeCurated = (mode: SpecBundle['mode']): ComputedCuratedRead => {
    const reviews = staged
      .filter((proposal) => proposal.review_decision)
      .map((proposal) => `${proposal.proposal_id}=${proposal.review_decision}`)
      .sort();
    let digest = 0;
    for (const char of reviews.join(',')) digest = (digest * 31 + char.charCodeAt(0)) >>> 0;
    const listed = staged
      .filter((proposal) => proposal.review_decision === 'accepted')
      .map((proposal) => ({
        ...UNVERIFIED_RECORD,
        record_id: proposal.proposal_id,
        gap_id: proposal.gap_id,
        source_id: proposal.source_id,
        target_id: proposal.target_id,
        edge_label: proposal.edge_label,
      }))
      .sort((left, right) => left.record_id.localeCompare(right.record_id));
    return {
      status: 'computed',
      export_mode: mode,
      view: 'curated',
      revision: `curated-revision-v1:${digest.toString(16).padStart(64, '0')}`,
      graph_basis: CURATED_GRAPH_BASIS,
      items: [],
      items_omitted: 0,
      listed,
      assertion_decisions: {
        digest: `curated-assertion-decisions-v1:${'0'.repeat(64)}`,
        rejections_applied: curation.filter((record) => record.decision === 'rejected').length,
        stale: [],
      },
      counts: { ...NO_COUNTS, accepted: listed.length, unverified: listed.length },
    };
  };
  let rejectJobAction = options.rejectJobActionOnce ?? false;
  reviewRequests = [];
  historyRequests = [];
  evidenceRequests = [];
  capturedDescriptionRequests = [];
  repoRootLookups = 0;
  investigationRequests = [];
  localHealthResponse = undefined;
  let investigation: InvestigationDetail | null = options.existingInvestigation ?? null;
  let startUncertainOnce = options.startUncertainOnce ?? false;
  const investigationJournal: InvestigationEvent[] = [];
  if (options.existingInvestigationEvents) investigationJournal.push(...options.existingInvestigationEvents);
  let investigationStep = 1;
  let latestInspectionTarget: InvestigationInspectionTarget | null = null;
  let straddleResult = options.straddleResultCommit ?? false;
  let savedNoticeChatReads = 0;
  releaseSavedNoticeChatRead = null;
  const savedNoticeChatGate = options.deferSavedNoticeChatRead
    ? new Promise<void>((resolve) => { releaseSavedNoticeChatRead = resolve; }) : null;
  const appendInvestigationEvent = (kind: InvestigationEvent['kind'], summary: string,
    tool: InvestigationEvent['tool'] = null) => {
    if (!investigation) throw new Error('Fixture task missing');
    const sequence = investigationJournal.length + 1;
    investigationJournal.push({ investigation_id: investigation.investigation_id, sequence, revision: sequence,
      kind, summary, tool, step_id: `step-${investigationStep}`, created_at: '2026-09-10T10:00:00Z' });
    investigation = { ...investigation, revision: sequence, last_event_sequence: sequence };
  };
  // The fake core boots with one queued job: the production surface offers
  // no job-creation control (AC-0077), so lifecycle stories act on it.
  let jobs: MockJob[] = [
    {
      id: 1,
      kind: 'ingest-source-v1:src_11111111111111111111111111111111',
      status: options.legacyRunningJob ? 'running' : 'queued',
      execution_tracking: options.legacyRunningJob ? 'legacy_unknown' : 'recorded',
      created_at: '2026-07-05T19:00:00Z',
      updated_at: '2026-07-05T19:00:00Z',
    },
  ];
  let curation: AssertionDecisionRecord[] = [];
  let graphStats = options.evidenceLessGraph ? { nodes: 1, edges: 0 } : { nodes: 42, edges: 99 };
  // Mirrors the #118 SettingsStore defaults and invariants.
  let tiers: MockTier[] = ['T1', 'T2', 'T3'].map((tier) => ({
    tier,
    enabled: tier !== 'T3',
    provider: 'local',
    consented: false,
    consent_disclosure: null,
    consented_at: null,
  }));
  const egressSummary = () => {
    const cloud = tiers.filter((t) => t.enabled && t.provider === 'cloud' && t.consented);
    return {
      cloud_tiers: cloud.map((t) => t.tier),
      bytes_sent: 0,
      label: cloud.length
        ? `Cloud enabled (${cloud.map((t) => t.tier).join(', ')}) · 0 bytes egress`
        : 'Local-only · 0 bytes egress',
    };
  };
  mockIPC(async (cmd, args) => {
    const savedExchanges = [...(options.additionalSavedParent ? [options.additionalSavedParent] : []), ...(options.additionalSavedExchanges ?? [])];
    const savedExchange = savedExchanges.find((item) => item.investigation_id === (args as { investigationId?: string } | undefined)?.investigationId);
    switch (cmd) {
      case 'check_local_ollama':
        investigationRequests.push({ command: cmd, args }); return localHealthResponse?.() ?? 'unverified';
      case 'investigation_specialists':
        return options.admissionSelection ? admissionCatalog : options.inspectionSelection ? inspectionCatalog : investigationCatalog;
      case 'list_investigations':
        return { items: [...(investigation ? [investigation] : []), ...savedExchanges], next_cursor: null };
      case 'start_investigation': {
        if (!options.investigationFlow) throw new Error('Investigation fixture disabled');
        investigationRequests.push({ command: cmd, args });
        const request = (args as { request: StartInvestigationRequest }).request;
        if (startUncertainOnce) { startUncertainOnce = false; throw new Error('Fixture start outcome was not confirmed'); }
        if (investigation && request.parent_id !== investigation.investigation_id) return investigation;
        if (investigation) investigationJournal.length = 0;
        investigation = investigationDetail(request.parent_id ? 'inv-app-follow-up' : 'inv-app-script', { question: request.question, scope: request.scope,
          parent_id: request.parent_id ?? null, conversation_id: request.conversation_id ?? 'conversation-inv-app-script',
          schema_version: request.schema_version, ...(request.schema_version === 2 ? {
            inspection_requirements: request.inspection_requirements, inspection_requirement_count: request.inspection_requirements.length,
            inspection_status: 'pending' as const } : {}),
          specialist_id: request.specialist_id, specialist: (options.inspectionSelection ? inspectionCatalog : investigationCatalog).specialists.find((item) => item.id === request.specialist_id)!,
          provider_mode: request.provider_mode, provider: investigationCatalog.providers.find((item) => item.mode === request.provider_mode)!,
          status: 'queued', graph_snapshot_id: null, has_result: false, revision: 0, last_event_sequence: 0,
          actions: { can_cancel: true, can_follow_up: false } });
        appendInvestigationEvent('created', 'Saved before context preparation.');
        jobs.push({ id: 91, kind: 'specialist-investigation', status: 'running', execution_tracking: 'recorded',
          investigation_id: investigation.investigation_id, created_at: investigation.created_at, updated_at: investigation.updated_at });
        return investigation;
      }
      case 'get_investigation': {
        if (savedExchange) return savedExchange;
        if (investigation?.status === 'queued') {
          appendInvestigationEvent('preparation_started', 'Preparing the frozen scoped graph.');
          appendInvestigationEvent('context_prepared', 'Admitted original graph identities.');
          appendInvestigationEvent('consent_required', 'The first cloud step requires consent.');
          investigation = { ...investigation!, status: 'awaiting_consent', graph_snapshot_id: 'snapshot:original' };
        }
        return investigation;
      }
      case 'investigation_events': {
        if (savedExchange) {
          const id = savedExchange.investigation_id;
          return { investigation_id: id, items: investigationEvents(id).filter((event) => event.sequence > (args as { afterSequence: number }).afterSequence), next_sequence: 9, has_more: false };
        }
        const after = (args as { afterSequence: number }).afterSequence;
        const items = investigationJournal.filter((event) => event.sequence > after).slice(0, 50);
        return { investigation_id: investigation?.investigation_id, items,
          next_sequence: items.at(-1)?.sequence ?? after, has_more: false };
      }
      case 'investigation_result':
        if (savedExchange) return investigationResult(savedExchange.investigation_id);
        if (investigation?.has_result && straddleResult) {
          straddleResult = false;
          return null;
        }
        return investigation?.has_result ? investigationResult(investigation.investigation_id) : null;
      case 'get_investigation_chat': {
        investigationRequests.push({ command: cmd, args });
        if (!options.savedNoticeChat) return null;
        if (investigation?.parent_id === options.savedNoticeChat.task_id &&
            investigation.investigation_id !== options.savedNoticeChat.task_id) {
          return savedNoticeChat(investigation.investigation_id, investigation.question, investigation.has_result);
        }
        savedNoticeChatReads += 1;
        if (savedNoticeChatGate && savedNoticeChatReads > (options.deferSavedNoticeChatAfterReads ?? 0)) {
          if (options.deferSavedNoticeChatAfterReads !== undefined) await new Promise<void>((resolve) => { releaseSavedNoticeChatRead = resolve; });
          else await savedNoticeChatGate;
        }
        if (savedExchange) {
          return savedNoticeChat(savedExchange.investigation_id, savedExchange.question);
        }
        return options.savedNoticeChatFirstReadPartial !== false && savedNoticeChatReads === 1
          ? { ...options.savedNoticeChat, messages: options.savedNoticeChat.messages.slice(0, 1) }
          : options.savedNoticeChat;
      }
      case 'investigation_consent':
        return investigation?.status === 'awaiting_consent'
          ? investigationConsent(investigation.investigation_id, investigation.revision, `step-${investigationStep}`) : null;
      case 'approve_investigation_step': {
        if (!investigation) throw new Error('Unknown investigation');
        const approved = args as { investigationId: string; stepId: string; revision: number; payloadHash: string };
        const pending = investigationConsent(investigation.investigation_id, investigation.revision, `step-${investigationStep}`);
        if (approved.investigationId !== pending.investigation_id || approved.stepId !== pending.step_id ||
            approved.revision !== pending.revision || approved.payloadHash !== pending.preview.payload_hash) throw new Error('Stale consent');
        investigationRequests.push({ command: cmd, args });
        appendInvestigationEvent('consent_approved', 'Consumed the grant for this invocation only.');
        appendInvestigationEvent('model_started', 'Started the approved invocation.');
        appendInvestigationEvent('model_completed', 'Received one bounded response.');
        if (investigationStep === 1) {
          appendInvestigationEvent('tool_started', 'Read original admitted citation C1.', 'read_evidence');
          appendInvestigationEvent('evidence_validation_reserved', 'Reserved the original full-file validation cost.', 'read_evidence');
          appendInvestigationEvent('tool_completed', 'Admitted the retained source span.', 'read_evidence');
          investigationStep = 2;
          appendInvestigationEvent('consent_required', 'New evidence requires approval of the next exact payload.');
        } else {
          appendInvestigationEvent('result_persisted', 'Saved findings against the original input ledger.');
          appendInvestigationEvent('completed', 'Execution completed; knowledge remains partial.');
          investigation = { ...investigation, status: 'completed', has_result: true,
            actions: { can_cancel: false, can_follow_up: true } };
          jobs = jobs.map((job) => job.id === 91 ? { ...job, status: 'done' } : job);
        }
        return investigation;
      }
      case 'decline_investigation_step':
      case 'cancel_investigation': {
        if (!investigation) throw new Error('Unknown investigation');
        investigationRequests.push({ command: cmd, args });
        appendInvestigationEvent('cancelled', 'The coordinator stopped this task.');
        investigation = { ...investigation, status: 'cancelled', cancel_requested: true,
          actions: { can_cancel: false, can_follow_up: true } };
        return investigation;
      }
      case 'read_investigation_citation': {
        investigationRequests.push({ command: cmd, args });
        const { investigationId, citationId } = args as { investigationId: string; citationId: string };
        const citation = investigationCitations.find((item) => item.citation_id === citationId)!;
        return { investigation_id: investigationId, citation_id: citationId, citation,
          status: citationId === 'C1' ? 'available' : citationId === 'C2' ? 'metadata_only' : 'working_tree_unverified',
          text: citationId === 'C1' ? 'ORIGINAL RETAINED APP FIXTURE SOURCE' : null };
      }
      case 'ping':
        return { app: 'cartograph', version: '0.0.1' };
      case 'graph_stats':
        return graphStats;
      case 'clear_graph':
        graphStats = { nodes: 0, edges: 0 };
        return graphStats;
      case 'list_jobs':
        return jobs;
      case 'list_job_usage':
        return [];
      case 'get_agent_pool_controls':
        return null;
      case 'list_nodes': {
        const label = (args as { label: string }).label;
        if (label === 'Endpoint') return [FAKE_ENDPOINT];
        if (label === 'Repo') {
          repoRootLookups += 1;
          return [FAKE_REPO];
        }
        return [];
      }
      case 'atlas_snapshot':
        return {
          nodes: [
            FAKE_ENDPOINT,
            {
              id: 'sym:app.ts#listUsers',
              label: 'Symbol',
              props: { name: 'listUsers', prov: FAKE_PROVENANCE },
            },
          ],
          edges: [
            {
              src: FAKE_ENDPOINT.id,
              dst: 'sym:app.ts#listUsers',
              label: 'HANDLES',
              props: { prov: FAKE_PROVENANCE },
            },
          ],
        };
      case 'read_evidence':
        evidenceRequests.push(args as Record<string, unknown>);
        if (options.unavailableEvidence) throw new Error('registered source unavailable');
        if (options.evidenceGate) return options.evidenceGate.then(() => ({ text: FAKE_SOURCE, window_start: 0, truncated: false }));
        return { text: FAKE_SOURCE, window_start: 0, truncated: false };
      case 'list_retained_sources':
        return [];
      case 'describe_captured_source': {
        if (!options.capturedDescriptions) return null;
        const request = args as Record<string, unknown>;
        capturedDescriptionRequests.push(request);
        return {
          graph_snapshot_id: options.admissionSelection ? completionChoices.graph_snapshot_id : 'snapshot:original',
          fact: request.fact as FactKey,
          receipt_id: `receipt:selection-${capturedDescriptionRequests.length}`,
          emitted_fact_digest: 'fact-digest', source_id: 'src_fixture', repo_key: 'local',
          ranges: [{ index: 0, path: 'src/app.ts', byte_start: SPAN_START, byte_end: SPAN_END }],
          scope: 'primary_source_only', input_closure: 'input_closure_not_established',
        } satisfies CapturedDescription;
      }
      case 'read_captured_source':
        return { receipt_id: `receipt:selection-${capturedDescriptionRequests.length}`, range_index: 0,
          text: SPAN_TEXT, path: 'src/app.ts', byte_start: SPAN_START, byte_end: SPAN_END };
      case 'resolve_investigation_inspection_target': {
        investigationRequests.push({ command: cmd, args });
        latestInspectionTarget = { ...inspectionTarget, graph_snapshot_id: options.admissionSelection ? completionChoices.graph_snapshot_id : inspectionTarget.graph_snapshot_id,
          target_id: 'c'.repeat(64), fact: (args as { fact: FactKey }).fact,
          fact_digest: 'fact-digest', registered_source_id: 'src_fixture',
          receipt_id: `receipt:selection-${capturedDescriptionRequests.length}`, receipt_inventory_index: 0,
          source: { ...inspectionTarget.source, repo: 'local', path: 'src/app.ts', byte_start: SPAN_START, byte_end: SPAN_END },
          captured: { ...inspectionTarget.captured, file: { ...inspectionTarget.captured.file, source_id: 'src_fixture', path: 'src/app.ts' },
            byte_start: SPAN_START, byte_end: SPAN_END } };
        return latestInspectionTarget;
      }
      case 'investigation_obligation_choices':
        return options.admissionSelection ? completionChoices : null;
      case 'resolve_investigation_source_obligation':
        investigationRequests.push({ command: cmd, args });
        if (!options.admissionSelection || !latestInspectionTarget) throw new Error('No selected completion source');
        return { kind: 'source', obligation_id: '6'.repeat(64), target: latestInspectionTarget };
      case 'export_flows':
        return '# Flow dossier\n\n## GET /users — Verified (score 1.00)\n';
      case 'list_flows':
        return [FAKE_FLOW];
      case 'list_flow_anchors':
        return [
          { kind: 'screens (routes/pages)', found: 0 },
          { kind: 'extension contexts (popup, background, content scripts)', found: 0 },
          { kind: 'extension commands (keyboard shortcuts)', found: 0 },
          { kind: 'HTTP endpoints', found: 1 },
          { kind: 'externally published event channels', found: 0 },
        ];
      case 'system_contents':
        return graphStats.nodes === 0 || options.evidenceLessGraph
          ? []
          : [{ repo: 'local/image-trail', commit: 'workdir' }];
      case 'list_plugins':
        return [];
      case 'set_plugin_enabled':
        return null;
      case 'run_plugin_gate':
        return { passed: true, checks: [] };
      case 'adapter_inventory':
        return {
          installed: [
            {
              id: 't0.adapter-ts',
              language: 'TypeScript/JavaScript',
              extensions: ['ts', 'tsx'],
              covers: 'imports, call graph, endpoints',
            },
          ],
          planned: [{ language: 'Swift', extensions: ['swift'] }],
          matrix: [],
          detector: 'preflight@1',
        };
      case 'export_topology':
        return 'flowchart LR\n    res_aws_sqs_queue_orders["aws_sqs_queue.orders"]\n';
      case 'export_spec':
        return { ...FAKE_SPEC, mode: (args as { mode: SpecBundle['mode'] }).mode };
      case 'export_spec_views':
        return fakeSpecViews((args as { mode: SpecBundle['mode'] }).mode);
      case 'export_spec_with_views': {
        const mode = (args as { mode: SpecBundle['mode'] }).mode;
        const curated = fakeCurated(mode);
        return {
          // As the host does: one projection per compile, returned beside
          // the bundle whose curated basis it gives.
          bundle: { ...FAKE_SPEC, mode, curated_basis: { revision: curated.revision, counts: curated.counts } },
          views: fakeSpecViews(mode),
          curated,
        };
      }
      case 'read_spec_artifact':
        return (
          FAKE_SPEC.artifacts.find(
            (artifact) => artifact.id === (args as { artifactId: string }).artifactId,
          ) ?? null
        );
      case 'read_spec_artifacts':
        return (args as { artifactIds: string[] }).artifactIds.map(
          (id) => FAKE_SPEC.artifacts.find((artifact) => artifact.id === id) ?? null,
        );
      case 'list_assertion_decisions':
        return curation;
      case 'record_assertion_decision': {
        const input = args as {
          assertion: AssertionDecisionRecord['assertion'];
          decision: AssertionDecisionRecord['decision'];
          note: string | null;
        };
        const record: AssertionDecisionRecord = {
          assertion: input.assertion,
          decision: input.decision,
          note: input.note,
          updated_at: '2026-07-13T18:00:00Z',
        };
        curation = [record];
        return record;
      }
      case 'findings_summary':
        // Derives from live graph state like the real core: a cleared graph
        // reports zero facts and the landing collapses honestly.
        return {
          gaps: graphStats.nodes > 0 ? 1 : 0,
          unsupported: 0,
          no_evidence: 0,
          drift: 0,
          open_findings: graphStats.nodes > 0 ? 1 : 0,
          graph_facts: graphStats.nodes + graphStats.edges,
        };
      case 'list_findings':
        return [];
      case 'list_evals':
        return [];
      case 'gap_strategies':
        return {
          gap_id: (args as { gapId: string }).gapId,
          summary: 'Gap: remote sync target computed at runtime',
          stop_reason: 'endpoint host computed from config at runtime',
          attempted_tiers: ['T0'],
          required_evidence: ['E1 · local:src/app.ts'],
          candidates: 1,
          strategies: [
            {
              id: 'local-slm',
              tier: 'T3',
              provider: 'ollama:qwen3:8b',
              locality: 'local',
              egress_bytes: 0,
              est_usd: null,
              latency: 'seconds on-device',
              privacy: 'payload never leaves the device',
              export_impact: 'An accepted proposal gets one state each time it is read. T3/InferredWeak is preserved.',
              available: true,
              unavailable_reason: null,
            },
          ],
        };
      case 'run_escalation': {
        const proposal = { ...stagedFixture(), gap_id: (args as { gapId: string }).gapId };
        if (options.unstagedResult) return { ...proposal, proposal_id: undefined };
        staged = [proposal, ...staged.filter((item) => item.proposal_id !== proposal.proposal_id)];
        return options.runGate ? options.runGate.then(() => proposal) : proposal;
      }
      case 'list_staged_proposals': {
        const input = args as { limit: number; cursor: string | null };
        historyRequests.push(input);
        const start = input.cursor ? staged.findIndex((item) => item.proposal_id === input.cursor) + 1 : 0;
        const items = staged.slice(start, start + Math.min(input.limit, options.pageSize ?? input.limit));
        const page = {
          items,
          next_cursor: start + items.length < staged.length ? items.at(-1)?.proposal_id ?? null : null,
        };
        return options.historyGate ? options.historyGate.then(() => page) : page;
      }
      case 'record_agent_decision': {
        const input = args as {
          proposalId: string; expectedRevision: number; decision: 'accepted' | 'rejected'; note: string | null;
        };
        reviewRequests.push({ ...input });
        if (options.reviewFailure === 'null') return null;
        const proposal = staged.find((item) => item.proposal_id === input.proposalId);
        if (!proposal) throw new Error('unknown staged proposal');
        if (options.reviewFailure === 'stale' || proposal.review_revision !== input.expectedRevision) {
          throw new Error('stale review revision; reload proposal history');
        }
        const reviewed = {
          ...proposal, review_revision: proposal.review_revision + 1,
          review_decision: input.decision, review_note: input.note,
          reviewed_at: '2026-09-10T13:00:00Z',
        };
        staged = staged.map((item) => item.proposal_id === reviewed.proposal_id ? reviewed : item);
        return options.reviewGate ? options.reviewGate.then(() => reviewed) : reviewed;
      }
      case 'extractor_coverage':
        return [
          {
            extractor: 't0.adapter-ts',
            files_in_scope: 2,
            files_with_facts: 1,
            facts: 12,
            coverage_pct: 50,
          },
        ];
      case 'ingest_history':
        return [
          {
            id: 2,
            job_id: 2,
            repo: 'local/fixture',
            commit_sha: 'workdir',
            confirmed: 40,
            inferred_strong: 2,
            inferred_weak: 0,
            gap: 1,
            unsupported: 0,
            no_evidence: 0,
            graph_facts: 43,
            content_hash: 'd'.repeat(64),
            created_at: '2026-07-14T11:00:00Z',
          },
          {
            id: 1,
            job_id: 1,
            repo: 'local/fixture',
            commit_sha: 'workdir',
            confirmed: 40,
            inferred_strong: 2,
            inferred_weak: 0,
            gap: 1,
            unsupported: 0,
            no_evidence: 0,
            graph_facts: 43,
            content_hash: 'd'.repeat(64),
            created_at: '2026-07-14T10:00:00Z',
          },
        ];
      case 'get_settings':
        return tiers;
      case 'egress_summary':
        return egressSummary();
      case 'get_ingest_parallelism':
        return { setting: 0, auto_workers: 7, workers: 7, max_workers: 12 };
      case 'cloud_disclosure': {
        const tier = (args as { tier: string }).tier;
        return {
          provider: 'Anthropic',
          model: tier === 'T2' ? 'claude-haiku-4-5-20251001' : 'claude-opus-4-8',
          endpoint: 'https://api.anthropic.com/v1/messages',
          input_usd_per_mtok: tier === 'T2' ? 1 : 5,
          output_usd_per_mtok: tier === 'T2' ? 5 : 25,
          notes: ['payload is the exact redacted span set shown by the egress firewall'],
        };
      }
      // Broker edge-label allowlist (#238), prefetched like the disclosures.
      case 'escalation_capabilities':
        return ['CALLS', 'PUBLISHES', 'READS', 'WRITES', 'HANDLES'];
      case 'set_tier_enabled': {
        const input = args as { tier: string; enabled: boolean };
        tiers = tiers.map((t) => (t.tier === input.tier ? { ...t, enabled: input.enabled } : t));
        return tiers;
      }
      case 'set_tier_provider': {
        const input = args as { tier: string; provider: string };
        // Leaving cloud revokes consent (SettingsStore invariant).
        tiers = tiers.map((t) =>
          t.tier === input.tier
            ? {
                ...t,
                provider: input.provider,
                consented: input.provider === 'cloud' ? t.consented : false,
                consent_disclosure:
                  input.provider === 'cloud' ? t.consent_disclosure : null,
                consented_at: input.provider === 'cloud' ? t.consented_at : null,
              }
            : t,
        );
        return tiers;
      }
      case 'grant_cloud_consent': {
        const input = args as { tier: string; disclosure: string };
        tiers = tiers.map((t) =>
          t.tier === input.tier
            ? {
                ...t,
                consented: true,
                consent_disclosure: input.disclosure,
                consented_at: '2026-07-14T20:00:00Z',
              }
            : t,
        );
        return tiers;
      }
      case 'revoke_cloud_consent': {
        const tier = (args as { tier: string }).tier;
        tiers = tiers.map((t) =>
          t.tier === tier
            ? { ...t, consented: false, consent_disclosure: null, consented_at: null }
            : t,
        );
        return tiers;
      }
      case 'preflight':
        return {
          languages: [
            { language: 'TypeScript/JavaScript', files: 9, adapter: 't0.adapter-ts' },
          ],
          frameworks: ['Chrome Extension MV3'],
          potential_gaps: [
            {
              kind: 'dynamic-injection',
              path: 'src/background.ts',
              line: 41,
              message: 'Dynamically injected function bodies (executeScript)',
              detector: 'preflight@1',
            },
          ],
          unsupported: [
            {
              kind: 'inline-eval',
              path: 'src/legacy.js',
              line: 120,
              message: 'Inline eval()',
              detector: 'preflight@1',
            },
          ],
          detector: 'preflight@1',
        };
      case 'preflight_system':
        // A manifest preview (#1655): a local member's own report, and a
        // remote member deferred to recovery.
        return {
          members: [
            {
              kind: 'local',
              entry: 'infra',
              path: '/fake/system-checkout/infra',
              report: {
                languages: [{ language: 'Terraform', files: 2, adapter: 't0.iac-terraform' }],
                frameworks: [],
                potential_gaps: [],
                unsupported: [],
                detector: 'preflight@1',
              },
            },
            { kind: 'deferred', entry: 'https://github.com/acme/shop' },
          ],
        };
      case 'ingest_path':
        return {
          job_id: 1,
          files: 2,
          nodes: 12,
          edges: 18,
          layers: {
            ts: { files: 1, nodes: 8, edges: 12 },
            python: { files: 0, nodes: 0, edges: 0 },
            go: { files: 0, nodes: 0, edges: 0 },
            tf: { files: 1, nodes: 4, edges: 6 },
            java: { files: 0, nodes: 0, edges: 0 },
            kotlin: { files: 0, nodes: 0, edges: 0 },
            webext: { files: 0, nodes: 0, edges: 0 },
            tools: { files: 0, nodes: 0, edges: 0 },
          },
          delta: { recomputed_files: 2, reused_files: 0, deleted_files: 0 },
        };
      case 'add_system':
        return {
          job_id: 3,
          repos: ['acme/shop@a1b2c3d4e5f6', 'local/infra@workdir'],
          files: 5,
          nodes: 40,
          edges: 60,
          layers: {
            ts: { files: 3, nodes: 25, edges: 38 },
            python: { files: 0, nodes: 0, edges: 0 },
            go: { files: 0, nodes: 0, edges: 0 },
            tf: { files: 2, nodes: 15, edges: 22 },
            java: { files: 0, nodes: 0, edges: 0 },
            kotlin: { files: 0, nodes: 0, edges: 0 },
            webext: { files: 0, nodes: 0, edges: 0 },
            tools: { files: 0, nodes: 0, edges: 0 },
          },
          delta: { recomputed_files: 5, reused_files: 0, deleted_files: 0 },
        };
      case 'add_repo':
        return {
          job_id: 2,
          repo: 'acme/shop',
          commit_sha: 'a'.repeat(40),
          files: 3,
          nodes: 20,
          edges: 30,
          layers: {
            ts: { files: 3, nodes: 20, edges: 30 },
            python: { files: 0, nodes: 0, edges: 0 },
            go: { files: 0, nodes: 0, edges: 0 },
            tf: { files: 0, nodes: 0, edges: 0 },
            java: { files: 0, nodes: 0, edges: 0 },
            kotlin: { files: 0, nodes: 0, edges: 0 },
            webext: { files: 0, nodes: 0, edges: 0 },
            tools: { files: 0, nodes: 0, edges: 0 },
          },
          delta: { recomputed_files: 3, reused_files: 0, deleted_files: 0 },
        };
      case 'cancel_job': {
        const id = (args as { id: number }).id;
        if (rejectJobAction) {
          rejectJobAction = false;
          throw new Error('execution ownership is busy');
        }
        jobs = jobs.map((job) => (job.id === id ? { ...job, status: 'cancelled' } : job));
        return jobs.find((job) => job.id === id);
      }
      case 'retry_job': {
        // The fake core mirrors #117's noop re-dispatch: re-queue then done.
        const id = (args as { id: number }).id;
        jobs = jobs.map((job) => (job.id === id ? { ...job, status: 'done' } : job));
        return jobs.find((job) => job.id === id);
      }
      case 'clear_finished_jobs': {
        const before = jobs.length;
        jobs = jobs.filter((job) => !['done', 'failed', 'cancelled'].includes(job.status));
        return before - jobs.length;
      }
      case 'provider_key_status':
        return [];
      case 'knowledge_phase_status':
        return null;
      case 'local_lane_status':
        return { reachable: false, start_command: 'ollama serve', models: [] };
      case 'section_document':
        // The Workbench document view (AC-0669): D1 with no stored draft.
        return sectionDocumentFixture([]);
      case 'recorded_coverage':
        return [];
      case 'area_choices':
        // Ask an area's expert (AC-0932): no knowledge overlay, so no areas.
        return [];
      case 'list_attested_records':
        // Attested records (AC-1081): none in the fixture project.
        return { items: [], next_before: null };
      default:
        throw new Error(`unmocked command: ${cmd}`);
    }
  });
}

const meta = {
  title: 'Shell/App',
  component: App,
  beforeEach: () => {
    // Fresh fake core and store per story run (module state persists between
    // stories otherwise); cleanup drops the fake __TAURI_INTERNALS__ so other
    // story files see a clean window.
    installFakeCore();
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    useSavedResultNoticeStore.setState(useSavedResultNoticeStore.getInitialState(), true);
    useAgentChatDockStore.setState({ docks: [], focusedTask: null, pendingTask: null, error: null, fallbackTask: null });
    useAppStore.setState({
      view: 'workspace',
      backend: 'unknown',
      version: null,
      stats: null,
      jobs: [],
      endpoints: [],
      atlas: { nodes: [], edges: [] },
      systemContents: [],
      flowList: [],
      specBundle: null,
      specViews: null,
      specMode: 'best-effort',
      curation: [],
      specBusy: false,
      specError: null,
      ingestBusy: false,
      ingestSummary: null,
      ingestError: null,
      ingestSource: 'github',
      ingestTarget: '',
      preflight: null,
      preflightBusy: false,
      preflightError: null,
      preflightProgress: null,
      clearBusy: false,
      clearError: null,
      findings: null,
      registerFindings: [],
      ingestHistory: [],
      coverage: [],
      evals: [],
      escalation: null,
      stagedProposals: [],
      stagedNextCursor: null,
      stagedLoading: false,
      stagedError: null,
      tierSettings: [],
      egress: null,
      disclosures: {},
      settingsError: null,
      recoverJobId: null,
      jobActionError: null,
      selected: null,
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof App>;

export default meta;
type Story = StoryObj<typeof meta>;

// Lazy route chunks may be cold in a fresh Storybook browser. Keep the first
// wait on a lazy surface explicitly bounded without making story order
// responsible for warming it; later waits keep the default so regressions
// still fail fast (#682).
const LAZY_ROUTE_WAIT = { timeout: 5_000 };
const waitForSpecWorkbench = (canvasElement: HTMLElement) =>
  waitFor(
    () => expect(within(canvasElement).getByText('9 artifacts')).toBeInTheDocument(),
    LAZY_ROUTE_WAIT,
  );

export const ConnectedToCore: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Boot: ping resolves and the status bar reports the core version.
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await expect(canvas.getByText('42')).toBeInTheDocument();
    await expect(canvas.getByText('99')).toBeInTheDocument();

    // Spec Workbench lives on its own surface now.
    await userEvent.click(canvas.getByRole('button', { name: 'Spec Workbench' }));
    await waitForSpecWorkbench(canvasElement);

    // Lifecycle round-trip (#117/#111) on the seeded queued job: cancel,
    // then retry. No creation control ships on the surface (AC-0077).
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await waitFor(() => expect(canvas.getByText('ingest-source-v1:src_11111111111111111111111111111111')).toBeInTheDocument());
    await expect(canvas.getByText('queued')).toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: /enqueue/i })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(canvas.getByText('cancelled')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(canvas.getByText('done')).toBeInTheDocument());

    // Clear finished round-trip (AC-0076): the now-done job is removable.
    await userEvent.click(canvas.getByRole('button', { name: 'Clear finished' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await waitFor(() => expect(canvas.getByText('No jobs yet.')).toBeInTheDocument());
  },
};

export const RejectedJobActionIsVisibleWithoutChangingHistory: Story = {
  // AC-0162: the real store catches rejection and the Jobs surface renders it;
  // a subsequent confirmed action updates the record and clears that error.
  beforeEach: () => installFakeCore({ rejectJobActionOnce: true }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    const before = useAppStore.getState().jobs;
    await userEvent.click(canvas.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(canvas.getByRole('alert')).toHaveTextContent('execution ownership is busy'));
    await expect(useAppStore.getState().jobs).toEqual(before);
    await expect(canvas.getByText('queued')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(canvas.getByText('cancelled')).toBeVisible());
    await expect(canvas.queryByRole('alert')).not.toBeInTheDocument();
  },
};

export const LegacyRunningJobCannotBecomeLiveRecovery: Story = {
  // AC-0162: real App wiring neither auto-selects nor animates a legacy row;
  // an explicit pin shows its stored history without substituting live work.
  beforeEach: () => installFakeCore({ legacyRunningJob: true }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    const history = useAppStore.getState().jobs;
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await expect(canvas.getByText('running')).toBeVisible();
    await expect(canvas.queryByRole('button', { name: 'View live' })).not.toBeInTheDocument();
    await expect(canvasElement.querySelector('.status-bar .spinning')).not.toBeInTheDocument();
    useAppStore.getState().setView('recover');
    await waitFor(() => expect(canvas.getByText('No active recovery selected')).toBeVisible());
    await expect(canvas.queryByTestId('recover-spinner')).not.toBeInTheDocument();
    useAppStore.getState().viewRecoveryJob(1);
    await waitFor(() => expect(canvas.getByText('Historical recovery record')).toBeVisible());
    await expect(canvas.getByText('Stored status: running')).toBeVisible();
    await expect(canvas.queryByTestId('recover-spinner')).not.toBeInTheDocument();
    await expect(useAppStore.getState().jobs).toEqual(history);
  },
};

export const IngestFlowEndToEnd: Story = {
  // #104 / US-0016: Connect → Preflight → Recover against the fake core.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    const breadcrumb = () => within(canvas.getByRole('navigation', { name: 'Breadcrumb' }));

    // Workspace → Connect. The ingest flow routes without joining the rail.
    // (The fake core boots with facts, so the landing offers Re-ingest.)
    await userEvent.click(canvas.getByRole('button', { name: /re-ingest/i }));
    await expect(breadcrumb().getByText('Connect')).toBeInTheDocument();
    await expect(
      canvas.getByText(/Nothing leaves the device unless you opt a tier into cloud/),
    ).toBeInTheDocument();

    // Local target → Preflight runs real (mocked-core) detection.
    await userEvent.click(canvas.getByRole('radio', { name: 'Local folder' }));
    await userEvent.type(canvas.getByRole('textbox'), '/fake/repo');
    await userEvent.click(canvas.getByRole('button', { name: /preflight/i }));
    await expect(breadcrumb().getByText('Preflight')).toBeInTheDocument();
    await waitFor(() =>
      expect(canvas.getByText('Potential system gaps')).toBeInTheDocument(),
    );
    // The three-way split renders from detection output, never conflated.
    await expect(
      canvas.getByText(/Dynamically injected function bodies \(executeScript\)/),
    ).toBeInTheDocument();
    await expect(canvas.getByText(/a tool limitation, not a System Gap/)).toBeInTheDocument();
    await expect(canvas.getByText('0 bytes egress')).toBeInTheDocument();

    // Run full recovery: the fake core resolves instantly, so the flow lands
    // back on Workspace with the recovery outcome visible.
    await userEvent.click(canvas.getByRole('button', { name: /run full recovery/i }));
    await waitFor(() => expect(breadcrumb().getByText('Workspace')).toBeInTheDocument());
    // The landing shows the honest outcome from the register summary.
    await waitFor(() =>
      expect(within(canvas.getByTestId('outcome-card')).getByText('1 open findings')).toBeInTheDocument(),
    );
  },
};

export const ManifestDirectoryUsesAddSystem: Story = {
  // The Connect step's explicit source wins over string inference: a manifest
  // target that is a *directory* (no cartograph.system.toml suffix) must
  // still dispatch add_system, never ingest_path (#104 review).
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());

    await userEvent.click(canvas.getByRole('button', { name: /re-ingest/i }));
    await userEvent.click(canvas.getByRole('radio', { name: 'System manifest' }));
    await userEvent.type(canvas.getByRole('textbox'), '/fake/system-checkout');
    await userEvent.click(canvas.getByRole('button', { name: /preflight/i }));
    // The manifest preview scans its local member and defers the remote one
    // to recovery rather than inventing a report (#1655).
    await waitFor(() =>
      expect(canvas.getByRole('region', { name: 'Repository infra' })).toBeInTheDocument(),
    );
    await expect(canvas.getByText('t0.iac-terraform')).toBeInTheDocument();
    const remote = canvas.getByRole('region', { name: 'Repository https://github.com/acme/shop' });
    await expect(within(remote).getByText('Detection deferred')).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: /run full recovery/i }));
    // add_system's summary lists every declared repo — the landing titles
    // itself from that multi-repo identity, proof the manifest loader ran
    // rather than the single-repo local path.
    await waitFor(() =>
      expect(canvas.getByText('2 repos as one system')).toBeInTheDocument(),
    );
  },
};

export const GapRowOpensEvidenceForEdgeGap: Story = {
  // #109 review fix: an edge gap has no atlas node, yet its register row
  // must still open the evidence drawer from the assertion's provenance.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Gaps & Drift' }));
    await waitFor(() =>
      expect(canvas.getByText(/callee not statically resolvable/)).toBeInTheDocument(),
    );
    await userEvent.click(canvas.getByText(/callee not statically resolvable/));
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
  },
};

export const EscalationRoundTrip: Story = {
  // #113/#120 end to end: gap row → Resolution Strategy → local run →
  // staged proposal → accept records the decision (propose-only, R-INT-3).
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Gaps & Drift' }));
    await waitFor(() =>
      expect(canvas.getByText(/remote sync target computed at runtime/)).toBeInTheDocument(),
    );

    await userEvent.click(canvas.getByText(/remote sync target computed at runtime/));
    await waitFor(() =>
      expect(
        canvas.getByRole('dialog', { name: /remote sync target/i }),
      ).toBeInTheDocument(),
    );
    await expect(
      canvas.getByText(/Why deterministic recovery stopped: endpoint host computed/),
    ).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Run locally' }));
    await waitFor(() => expect(canvas.getByTestId('proposal-card')).toBeInTheDocument());
    await expect(canvas.getByText(/An accepted proposal gets one state each time it is read: only/)).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Accept as InferredWeak' }));
    await waitFor(() =>
      expect(canvas.getByTestId('decision-recorded')).toHaveTextContent(
        'Decision recorded: accepted',
      ),
    );
  },
};

export const FlowHopsRouteByKind: Story = {
  // #107 end to end: on the Flows surface a confirmed hop card opens the
  // evidence drawer from the hop's own provenance, while a Gap hop card
  // opens the Resolution Strategy modal for its Gap node.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Flows' }));
    await waitFor(
      () => expect(canvas.getByText('PARTIAL (1 gap)')).toBeInTheDocument(),
      LAZY_ROUTE_WAIT,
    );

    await userEvent.click(
      canvas.getByRole('button', { name: 'HANDLES: GET /users to listUsers' }),
    );
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
    await userEvent.keyboard('{Escape}');

    await userEvent.click(canvas.getByRole('button', { name: /Unresolved hop/ }));
    await waitFor(() =>
      expect(
        canvas.getByRole('dialog', { name: /remote sync target/i }),
      ).toBeInTheDocument(),
    );
  },
};

export const SettingsConsentRoundTrip: Story = {
  // #112 / US-0009: opt T2 into cloud, grant consent, watch the status bar
  // flip — then revoke and watch it fail closed again.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    const statusBar = () => within(canvasElement.querySelector('.status-bar') as HTMLElement);
    await expect(statusBar().getByText('Local-only · 0 bytes egress')).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Settings' }));
    await waitFor(() =>
      expect(canvas.getByRole('switch', { name: 'Semantic enabled' })).toBeInTheDocument(),
    );

    // Cloud selection reveals the full disclosure before consent exists.
    const t2Group = within(canvas.getByRole('radiogroup', { name: 'T2 provider' }));
    await userEvent.click(t2Group.getByRole('radio', { name: 'Cloud (opt-in)' }));
    await waitFor(() =>
      expect(canvas.getByText('Cloud egress consent — T2')).toBeInTheDocument(),
    );
    await expect(canvas.getByText('claude-haiku-4-5-20251001')).toBeInTheDocument();
    // Still consentless: the status bar has not moved.
    await expect(statusBar().getByText('Local-only · 0 bytes egress')).toBeInTheDocument();

    // Grant: the summary derives from settings state everywhere.
    await userEvent.click(canvas.getByRole('button', { name: 'Grant revocable consent' }));
    await waitFor(() =>
      expect(
        statusBar().getByText('Cloud enabled (T2) · 0 bytes egress'),
      ).toBeInTheDocument(),
    );
    await expect(canvas.getByText(/Standing cloud consent granted/)).toBeInTheDocument();

    // Revoke: immediate, and the shell reads local-only again.
    await userEvent.click(canvas.getByRole('button', { name: 'Revoke consent' }));
    await waitFor(() =>
      expect(statusBar().getByText('Local-only · 0 bytes egress')).toBeInTheDocument(),
    );
  },
};

export const ShellNavigation: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    const breadcrumb = () => within(canvas.getByRole('navigation', { name: 'Breadcrumb' }));

    // ⌘K opens the palette; Esc closes it without navigating.
    await userEvent.keyboard('{Meta>}k{/Meta}');
    await expect(canvas.getByRole('dialog', { name: 'Command palette' })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    await waitFor(() =>
      expect(canvas.queryByRole('dialog', { name: 'Command palette' })).not.toBeInTheDocument(),
    );
    await expect(breadcrumb().getByText('Workspace')).toBeInTheDocument();

    // Palette navigation: ⌘K → pick a surface.
    await userEvent.keyboard('{Meta>}k{/Meta}');
    await userEvent.click(canvas.getByRole('option', { name: /Gaps & Drift/ }));
    await expect(breadcrumb().getByText('Gaps & Drift')).toBeInTheDocument();

    // ⌘-digit shortcuts jump directly.
    await userEvent.keyboard('{Meta>}3{/Meta}');
    await expect(breadcrumb().getByText('Flows')).toBeInTheDocument();

    // Every rail surface renders — no dead or crashing route (handoff #2).
    for (const label of [
      'Workspace',
      'Atlas',
      'Flows',
      'Spec Workbench',
      'Gaps & Drift',
      'Provenance & Eval',
      'Jobs',
      'Settings',
    ]) {
      await userEvent.click(canvas.getByRole('button', { name: label }));
      await expect(breadcrumb().getByText(label)).toBeInTheDocument();
      await expect(canvas.queryByRole('alert')).not.toBeInTheDocument();
    }

    // The Legend popover is reachable from the header.
    await userEvent.click(canvas.getByRole('button', { name: /legend/i }));
    await expect(
      canvas.getByRole('dialog', { name: 'Tier, shape, and edge legend' }),
    ).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Close legend' }));
  },
};

export const AgentChatsDestinationPreservesInvestigationDraft: Story = {
  // AC-0388: the forum destination is a read-only saved-task surface; it must
  // not dispatch work or replace the existing composer/retry request.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const initial = useInvestigationStore.getState();
    const composer = { ...initial.composer, specialistId: 'evidence-auditor@3' as const,
      question: 'Keep this investigation draft.' };
    const pendingStart = { schema_version: 1 as const, request_nonce: 'preserved-agent-chat-nonce',
      specialist_id: 'domain-analyst@3' as const, question: 'Keep this exact retry request.',
      scope: { type: 'all' as const }, provider_mode: 'local' as const, limit_profile: 'investigation-v1' };
    useInvestigationStore.setState({ composer, pendingStart });
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Agent Chats' })).toBeVisible());
    await expect(canvas.getByText('No saved tasks loaded.')).toBeVisible();
    await expect(useInvestigationStore.getState().composer).toEqual(composer);
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pendingStart);
    await expect(investigationRequests).toHaveLength(0);

    await userEvent.click(canvas.getByRole('button', { name: 'Command palette' }));
    const palette = within(canvas.getByRole('dialog', { name: 'Command palette' }));
    const chats = palette.getByRole('option', { name: /^Agent Chats/ });
    await expect(chats.querySelector('kbd')).toBeNull();
    await userEvent.click(chats);
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Agent Chats' })).toBeVisible());
    await expect(useInvestigationStore.getState().composer).toEqual(composer);
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pendingStart);
    await expect(investigationRequests).toHaveLength(0);

    await userEvent.click(canvas.getByRole('button', { name: 'Command palette' }));
    const returnPalette = within(canvas.getByRole('dialog', { name: 'Command palette' }));
    await userEvent.click(returnPalette.getByRole('option', { name: /^Investigations/ }));
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Investigations' })).toBeVisible());
    await expect(useInvestigationStore.getState().composer).toEqual(composer);
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pendingStart);
    await expect(investigationRequests).toHaveLength(0);
  },
};

export const ConfirmedQuickDispatchOpensExactAgentChat: Story = {
  // AC-0391: only the confirmed task identity is handed to the existing chat
  // coordinator; the saved question and composer/unread state stay untouched.
  beforeEach: () => installFakeCore({ investigationFlow: true, startUncertainOnce: true, deferSavedNoticeChatRead: true,
    savedNoticeChat: savedNoticeChat('inv-app-script', 'Open the accepted saved task.', false) }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const initial = useInvestigationStore.getState();
    const composer = { ...initial.composer, question: 'Keep the unrelated composer draft.' };
    const existingUnread = [{ task_id: 'inv-other-unread', result_id: 'result-other-unread', question: 'Keep this unread notice.' }];
    useInvestigationStore.setState({ composer });
    useSavedResultNoticeStore.setState({ unread: existingUnread });
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());

    fireEvent.keyDown(canvas.getByRole('button', { name: 'Agent Chats' }), { key: ' ', code: 'Space', metaKey: true, bubbles: true });
    await userEvent.type(canvas.getByLabelText('Question'), 'Open the accepted saved task.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await expect(canvas.getByText(/outcome is uncertain/)).toBeVisible();
    await expect(useAppStore.getState().view).toBe('workspace');
    await expect(useAgentChatDockStore.getState().pendingTask).toBeNull();
    await expect(useAgentChatDockStore.getState().docks).toHaveLength(0);
    await expect(investigationRequests.filter((request) => request.command === 'start_investigation')).toHaveLength(1);
    await userEvent.click(canvas.getByRole('button', { name: 'Close quick investigation' }));
    fireEvent.keyDown(canvas.getByRole('button', { name: 'Agent Chats' }), { key: ' ', code: 'Space', metaKey: true, bubbles: true });
    await userEvent.click(canvas.getByRole('button', { name: 'Retry same request' }));
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Agent Chats' })).toBeVisible());
    await waitFor(() => expect(investigationRequests.filter((request) => request.command === 'get_investigation_chat')).toHaveLength(1));
    expect(useAgentChatDockStore.getState().docks).toHaveLength(0);
    expect(investigationRequests.filter((request) => request.command === 'get_investigation_chat')).toHaveLength(1);
    releaseSavedNoticeChatRead?.();
    await waitFor(() => expect(useAgentChatDockStore.getState().docks.map((item) => item.chat.task_id)).toEqual(['inv-app-script']));

    const pane = within(canvas.getByRole('region', { name: 'Saved chat inv-app-script' }));
    await expect(pane.getByRole('listitem', { name: 'User question' })).toHaveTextContent('Open the accepted saved task.');
    await expect(useInvestigationStore.getState().selectedId).toBe('inv-app-script');
    await expect(useAgentChatDockStore.getState().docks[0].chat.chat_id).toBe('saved-chat:inv-app-script');
    const starts = investigationRequests.filter((request) => request.command === 'start_investigation');
    await expect(starts).toHaveLength(2);
    expect(starts[0]).toEqual(starts[1]);
    expect((starts[0].args as { request: StartInvestigationRequest }).request.request_nonce).toMatch(/^[0-9a-f-]{36}$/);
    // Once the held handoff read settles, active-task polling may refresh the same chat.
    const chatReads = investigationRequests.filter((request) => request.command === 'get_investigation_chat');
    expect(chatReads.every((request) => (request.args as { investigationId: string }).investigationId === 'inv-app-script')).toBe(true);
    await expect(useInvestigationStore.getState().composer).toEqual(composer);
    await expect(useInvestigationStore.getState().pendingStart).toBeNull();
    await expect(useSavedResultNoticeStore.getState().unread).toEqual(existingUnread);
    expect(investigationRequests.some((request) => ['cancel_investigation', 'delete_investigation'].includes(request.command))).toBe(false);
  },
};

export const ConfirmedQuickDispatchOpensChatWhileDestinationIsConnected: Story = {
  // The same handoff opens immediately when Agent Chats is already mounted.
  beforeEach: () => installFakeCore({ investigationFlow: true,
    savedNoticeChat: savedNoticeChat('inv-app-script', 'Open the accepted saved task.', false) }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Agent Chats' })).toBeVisible());

    fireEvent.keyDown(canvas.getByRole('button', { name: 'Agent Chats' }), { key: ' ', code: 'Space', metaKey: true, bubbles: true });
    await userEvent.type(canvas.getByLabelText('Question'), 'Open the accepted saved task.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await waitFor(() => expect(useAgentChatDockStore.getState().docks.map((item) => item.chat.task_id)).toEqual(['inv-app-script']));
    await expect(canvas.getByRole('heading', { name: 'Agent Chats' })).toBeVisible();
    await expect(useAgentChatDockStore.getState().focusedTask).toBe('inv-app-script');
    await expect(useInvestigationStore.getState().composer.question).toBe('');
    expect(investigationRequests.some((request) => request.command === 'start_investigation')).toBe(true);
    expect(investigationRequests.filter((request) => request.command === 'get_investigation_chat')
      .every((request) => (request.args as { investigationId: string }).investigationId === 'inv-app-script')).toBe(true);
  },
};

export const SavedChatFollowUpUsesItsValidatedParent: Story = {
  // AC-0392..0394: the clicked pane supplies the frozen parent identity;
  // dispatch preserves unrelated composer state and uses the existing task flow.
  beforeEach: () => {
    const parentId = 'inv-follow-up-parent';
    const parent = investigationDetail(parentId, { question: 'Inspect this saved parent task.', status: 'completed',
      has_result: true, actions: { can_cancel: false, can_follow_up: true } });
    installFakeCore({ investigationFlow: true, existingInvestigation: parent,
      existingInvestigationEvents: investigationEvents(parentId), savedNoticeChat: savedNoticeChat(parentId, parent.question),
      savedNoticeChatFirstReadPartial: false });
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const before = useInvestigationStore.getState();
    const composer = { ...before.composer, question: 'Leave the full composer draft unchanged.', parent: {
      conversation_id: 'unrelated-conversation', parent_id: 'unrelated-parent' } };
    useInvestigationStore.setState({ composer });
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Agent Chats' })).toBeVisible());
    await userEvent.click(await canvas.findByRole('button', { name: /Inspect this saved parent task/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat inv-follow-up-parent' }));
    await userEvent.click(pane.getByRole('button', { name: 'Follow-up task' }));
    const dialog = await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    await expect(dialog).toHaveTextContent('Following inv-follow-up-parent in conversation conversation-inv-follow-up-parent');
    await useInvestigationStore.getState().open('unrelated-global-selection');
    await expect(useInvestigationStore.getState().selectedId).toBe('unrelated-global-selection');
    await userEvent.type(canvas.getByLabelText('Question'), 'Check the saved parent result against current context.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up task' }));
    await waitFor(() => expect(investigationRequests.filter((item) => item.command === 'start_investigation')).toHaveLength(1));
    await waitFor(() => expect(useAgentChatDockStore.getState().docks.map((item) => item.chat.task_id)).toContain('inv-app-follow-up'));
    const request = (investigationRequests.find((item) => item.command === 'start_investigation')!.args as { request: StartInvestigationRequest }).request;
    expect(request).toMatchObject({ parent_id: 'inv-follow-up-parent', conversation_id: 'conversation-inv-follow-up-parent',
      specialist_id: 'domain-analyst@3', scope: { type: 'neighborhood', anchor: 'rule:stock', hops: 1 },
      question: 'Check the saved parent result against current context.' });
    await expect(useInvestigationStore.getState().composer).toEqual(composer);
    const childPane = within(await canvas.findByRole('region', { name: 'Saved chat inv-app-follow-up' }));
    await expect(childPane.getByRole('listitem', { name: 'User question' })).toHaveTextContent('Check the saved parent result against current context.');
  },
};

export const CancelledParentFollowUpOpensStandaloneQuickDispatch: Story = {
  beforeEach: () => {
    const parentId = 'inv-follow-up-parent';
    const parent = investigationDetail(parentId, { question: 'Inspect this saved parent task.', status: 'completed',
      has_result: true, actions: { can_cancel: false, can_follow_up: true } });
    installFakeCore({ investigationFlow: true, existingInvestigation: parent,
      existingInvestigationEvents: investigationEvents(parentId), savedNoticeChat: savedNoticeChat(parentId, parent.question),
      savedNoticeChatFirstReadPartial: false });
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Inspect this saved parent task/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat inv-follow-up-parent' }));
    await userEvent.click(pane.getByRole('button', { name: 'Follow-up task' }));
    await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    await userEvent.type(canvas.getByLabelText('Question'), 'Check the captured parent.');
    await expect(canvas.getByRole('button', { name: 'Start follow-up task' })).toBeEnabled();
    fireEvent.click(pane.getByRole('button', { name: 'Close inv-follow-up-parent' }));
    fireEvent.click(canvas.getByRole('button', { name: 'Start follow-up task' }));
    await expect(investigationRequests.filter((request) => request.command === 'start_investigation')).toHaveLength(0);
    await waitFor(() => expect(canvas.queryByRole('dialog', { name: 'Quick investigation dispatch' })).not.toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Settings' }));
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Settings' })).toBeVisible());
    await userEvent.click(canvas.getByRole('button', { name: 'Open quick investigation' }));
    const standalone = await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    await expect(standalone).toHaveTextContent('Quick investigation');
    await expect(standalone).not.toHaveTextContent('Following inv-follow-up-parent');
    await expect(canvas.getByLabelText('Question')).toHaveValue('');

    await userEvent.click(canvas.getByRole('button', { name: 'Close quick investigation' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Inspect this saved parent task/ }));
    const retryPane = within(await canvas.findByRole('region', { name: 'Saved chat inv-follow-up-parent' }));
    await userEvent.click(retryPane.getByRole('button', { name: 'Follow-up task' }));
    await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' });
    const retainedRetry = { schema_version: 1 as const, request_nonce: 'b'.repeat(36), specialist_id: 'domain-analyst@3' as const,
      question: 'Keep this parent-bound retry.', scope: { type: 'neighborhood' as const, anchor: 'rule:stock', hops: 1 as const },
      provider_mode: 'local' as const, limit_profile: 'investigation-v1' as const,
      conversation_id: 'conversation-inv-follow-up-parent', parent_id: 'inv-follow-up-parent' };
    useInvestigationStore.setState({ pendingStart: retainedRetry });
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Retry same request' })).toBeEnabled());
    fireEvent.click(retryPane.getByRole('button', { name: 'Close inv-follow-up-parent' }));
    fireEvent.click(canvas.getByRole('button', { name: 'Retry same request' }));
    await expect(investigationRequests.filter((request) => request.command === 'start_investigation')).toHaveLength(0);
    await expect(useInvestigationStore.getState().pendingStart).toEqual(retainedRetry);
  },
};

const noticedTaskId = 'inv-notice-chat';
const noticedQuestion = 'Open the saved specialist conversation for this task.';
const noticedDetail = investigationDetail(noticedTaskId, { question: noticedQuestion, status: 'completed', has_result: true });

export const SavedResultNoticeOpensExactChatWithoutChangingComposer: Story = {
  // AC-0390: the notice carries only its task identity; Agent Chats obtains
  // the validated persisted transcript through its existing coordinator.
  beforeEach: () => installFakeCore({ existingInvestigation: noticedDetail,
    existingInvestigationEvents: investigationEvents(noticedTaskId),
    savedNoticeChat: savedNoticeChat(noticedTaskId, noticedQuestion) }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const rail = () => canvas.getByRole('button', { name: 'Agent Chats' });
    const badge = () => rail().querySelector('.nav-saved-result-badge');
    await expect(badge()).toBeNull();
    const initial = useInvestigationStore.getState();
    const composer = { ...initial.composer, question: 'Preserve this unrelated draft.' };
    const pendingStart = { schema_version: 1 as const, request_nonce: 'notice-chat-preserved-nonce',
      specialist_id: 'domain-analyst@3' as const, question: 'Preserve this retry exactly.',
      scope: { type: 'all' as const }, provider_mode: 'local' as const, limit_profile: 'investigation-v1' };
    useInvestigationStore.setState({ composer, pendingStart });
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());

    const notices = useSavedResultNoticeStore.getState();
    const event = { investigation_id: noticedTaskId, revision: 1, last_event_sequence: 9 };
    notices.observe(event);
    await waitFor(() => expect(investigationRequests.filter((request) =>
      request.command === 'get_investigation_chat')).toHaveLength(1));
    notices.observe({ ...event, revision: 2 });
    const openChat = () => canvas.getByRole('button', { name: `Open chat for ${noticedTaskId}: ${noticedQuestion}` });
    await waitFor(() => expect(openChat()).toBeInTheDocument());
    expect(useSavedResultNoticeStore.getState().unread).toHaveLength(1);
    await expect(badge()?.textContent).toBe('1');
    await expect(rail()).toHaveAccessibleDescription('Known unread saved results: 1.');
    const beforeBadgeChanges = [...investigationRequests];
    useSavedResultNoticeStore.setState({ incomplete: true });
    await waitFor(() => expect(badge()?.textContent).toBe('1?'));
    useSavedResultNoticeStore.setState({ incomplete: false });
    await waitFor(() => expect(badge()?.textContent).toBe('1'));
    await userEvent.click(canvas.getByRole('button', { name: 'Dismiss saved result notice' }));
    await expect(badge()?.textContent).toBe('1');
    expect(useSavedResultNoticeStore.getState().unread).toHaveLength(1);
    await userEvent.click(canvas.getByRole('button', { name: /New results/ }));
    expect(investigationRequests).toEqual(beforeBadgeChanges); // badge state and dismissal add no IPC

    await userEvent.click(openChat());
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Agent Chats' })).toBeVisible());
    await waitFor(() => expect(useAgentChatDockStore.getState().docks.map((item) => item.chat.chat_id))
      .toEqual([`saved-chat:${noticedTaskId}`]));
    expect(investigationRequests.filter((request) => request.command === 'get_investigation_chat')
      .every((request) => request.args && (request.args as { investigationId: string }).investigationId === noticedTaskId)).toBe(true);
    expect(investigationRequests.some((request) => ['start_investigation', 'cancel_investigation', 'delete_investigation']
      .includes(request.command))).toBe(false);
    expect(useSavedResultNoticeStore.getState().unread).toHaveLength(1); // transcript open is not findings-read
    await expect(badge()?.textContent).toBe('1');
    await expect(rail()).toHaveAttribute('aria-current', 'page');
    expect(useInvestigationStore.getState().composer).toEqual(composer);
    expect(useInvestigationStore.getState().pendingStart).toEqual(pendingStart);

    await userEvent.click(canvas.getByRole('button', { name: `Open findings for ${noticedTaskId}: ${noticedQuestion}` }));
    await waitFor(() => expect(canvas.getByRole('heading', { name: 'Investigations' })).toBeVisible());
    await waitFor(() => expect(useSavedResultNoticeStore.getState().unread).toEqual([]));
    await expect(badge()).toBeNull();
    await expect(rail()).not.toHaveAttribute('aria-describedby');
    expect(useInvestigationStore.getState().composer).toEqual(composer);
    expect(useInvestigationStore.getState().pendingStart).toEqual(pendingStart);
  },
};

export const EvidenceJumpToSource: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // M1 exit gate, end to end: recovered endpoint -> evidence -> source span.
    await waitFor(() => expect(canvas.getByText('/users')).toBeInTheDocument());
    await userEvent.click(canvas.getByText('/users'));
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
    const evidence = within(canvasElement.querySelector('.evidence-panel') as HTMLElement);
    await expect(evidence.getByText(/t0\.adapter-ts/)).toBeInTheDocument();
    // AC-0143: the host receives the exact citation identity, never a UI root.
    await expect(evidenceRequests).toEqual([{
      repo: 'local', path: 'src/app.ts', byteStart: SPAN_START, byteEnd: SPAN_END,
    }]);
    await expect(repoRootLookups).toBe(0);
    await expect(evidence.getByTestId('source-revision-status')).toHaveTextContent(
      'Current working-tree source — cited revision unverified.',
    );

    // Close returns to the dashboard.
    await userEvent.click(canvas.getByRole('button', { name: /close/i }));
    await waitFor(() =>
      expect(canvasElement.querySelector('.evidence-panel')).not.toBeInTheDocument(),
    );
  },
};

export const UnavailableEvidenceNeverFallsBackToAnotherRepository: Story = {
  // AC-0143: an unrelated Repo root must never substitute for unavailable or
  // historical evidence. Keep provenance visible after the host rejects a read.
  beforeEach: () => installFakeCore({ unavailableEvidence: true }),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('/users')).toBeInTheDocument());
    await userEvent.click(canvas.getByText('/users'));
    const drawer = within(canvas.getByTestId('evidence-drawer'));
    await waitFor(() => expect(drawer.getByText(/Source unavailable for this repository/)).toBeInTheDocument());
    await expect(evidenceRequests).toEqual([{
      repo: 'local', path: 'src/app.ts', byteStart: SPAN_START, byteEnd: SPAN_END,
    }]);
    await expect(repoRootLookups).toBe(0);
    await expect(drawer.queryByTestId('evidence-code')).not.toBeInTheDocument();
    await expect(drawer.queryByTestId('source-revision-status')).not.toBeInTheDocument();
    await expect(drawer.getByText('local:src/app.ts')).toBeInTheDocument();
    await expect(drawer.getByText(`bytes ${SPAN_START}–${SPAN_END}`)).toBeInTheDocument();
    await expect(drawer.getByText('workdir')).toBeInTheDocument();
    await expect(drawer.getByTestId('content-hash')).toHaveTextContent(FAKE_PROVENANCE.content_hash);
  },
};

export const SameFactReselectionRefreshesCapturedSource: Story = {
  // AC-0154: exercise the real App effect; same-object reselection requests a
  // new receipt, while settling its live-source request must not restart it.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const endpoints = canvas.getByRole('heading', { name: 'Endpoints' }).closest('section') as HTMLElement;
    const endpoint = await within(endpoints).findByRole('button', { name: /GET \/users/ });
    let releaseEvidence = () => {};
    const evidenceGate = new Promise<void>((resolve) => { releaseEvidence = resolve; });
    installFakeCore({ capturedDescriptions: true, evidenceGate });
    usePrimarySourceStore.getState().clear();
    try {
      await userEvent.click(endpoint);
      await waitFor(() => expect(capturedDescriptionRequests).toHaveLength(1));
      const subject = useAppStore.getState().selected?.node;
      const firstToken = useAppStore.getState().selected?.requestVersion;
      await userEvent.click(endpoint);
      await waitFor(() => expect(capturedDescriptionRequests).toHaveLength(2));
      await expect(useAppStore.getState().selected?.node).toBe(subject);
      await expect(useAppStore.getState().selected?.requestVersion).not.toBe(firstToken);
      await expect(capturedDescriptionRequests[1]).toEqual({
        fact: { kind: 'node', id: FAKE_ENDPOINT.id }, expectedNode: FAKE_ENDPOINT, expectedEdge: null,
      });
      await waitFor(() => expect(usePrimarySourceStore.getState().description?.receipt_id).toBe('receipt:selection-2'));
      releaseEvidence();
      await waitFor(() => expect(canvasElement.querySelector('.evidence-source mark')?.textContent).toBe(SPAN_TEXT));
      // Let both React's passive effect turn and a following render settle;
      // the completed live read changes selected.source, never its token.
      await userEvent.click(canvas.getByText('Capture reference'));
      await expect(capturedDescriptionRequests).toHaveLength(2);
      await expect(usePrimarySourceStore.getState().description?.receipt_id).toBe('receipt:selection-2');
    } finally {
      releaseEvidence();
      useAppStore.getState().clearSelection();
      usePrimarySourceStore.getState().clear();
    }
  },
};

export const AtlasNodeToEvidence: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Atlas' }));
    await waitFor(
      () =>
        expect(within(canvas.getByLabelText('Confidence legend')).getByRole('status')).toHaveTextContent(
          '2 nodes · 1 edges',
        ),
      LAZY_ROUTE_WAIT,
    );
    await userEvent.click(canvas.getByRole('button', { name: /^GET \/users$/ }));
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
    const evidence = within(canvasElement.querySelector('.evidence-panel') as HTMLElement);
    await expect(evidence.getByText(/src\/app\.ts/)).toBeInTheDocument();
    await expect(evidence.getByText(/workdir/)).toBeInTheDocument();

    // #106: with the drawer open, ONE click on an edge chip re-selects —
    // the overlay never swallows it (handoff interaction #1) — and the
    // drawer now shows the edge's own evidence.
    await userEvent.click(canvas.getByRole('button', { name: 'T0 HANDLES: ep:GET:/users to sym:app.ts#listUsers' }));
    await waitFor(() =>
      expect(
        within(canvasElement.querySelector('.evidence-panel') as HTMLElement).getByText(
          /ep:GET:\/users HANDLES sym:app\.ts#listUsers/,
        ),
      ).toBeInTheDocument(),
    );
  },
};

export const WorkbenchCurationRoundTrip: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Spec Workbench' }));
    await waitForSpecWorkbench(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: /Architecture decisions/ }));
    await expect(canvas.getByText(FAKE_INFERRED.summary)).toBeInTheDocument();
    await userEvent.type(canvas.getByLabelText('Annotation'), 'Confirmed by system owner');
    await userEvent.click(canvas.getByRole('button', { name: 'Annotate' }));
    await waitFor(() => expect(canvas.getAllByText('Annotated').length).toBeGreaterThan(0));
    await expect(canvas.getAllByText('Confirmed by system owner').length).toBeGreaterThan(0);
    await expect(canvas.getAllByText('Inferred (strong)').length).toBeGreaterThan(0);
  },
};

export const EvidenceLessGraphNamesNoSystem: Story = {
  beforeEach: () => installFakeCore({ evidenceLessGraph: true }),
  play: async ({ canvasElement }) => {
    // AC-0085: a persisted synthetic Repo node has no supporting evidence.
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByTestId('graph-node-count')).toHaveTextContent('1'));
    await expect(useAppStore.getState().ingestSummary).toBeNull();
    const breadcrumb = within(canvas.getByRole('navigation', { name: 'Breadcrumb' }));
    await expect(breadcrumb.getByText('No system')).toBeVisible();
    await expect(breadcrumb.queryByText('Ingested system')).not.toBeInTheDocument();
    await useAppStore.getState().refresh();
    await expect(breadcrumb.getByText('No system')).toBeVisible();
  },
};

export const ClearGraphPreservesJobs: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByTestId('graph-node-count')).toHaveTextContent('42'));
    // AC-0085: cold UI startup reads a saved graph, not an ingest response.
    await expect(useAppStore.getState().ingestSummary).toBeNull();
    const breadcrumb = within(canvas.getByRole('navigation', { name: 'Breadcrumb' }));
    // #2469: the breadcrumb names the persisted system after a relaunch.
    await expect(breadcrumb.getByText('local/image-trail')).toBeVisible();
    await expect(breadcrumb.queryByText('Ingested system')).not.toBeInTheDocument();

    // The seeded job exists, then clear the graph from Workspace: the
    // durable job spine must survive a graph clear.
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await waitFor(() => expect(canvas.getByText('ingest-source-v1:src_11111111111111111111111111111111')).toBeInTheDocument());

    await userEvent.click(canvas.getByRole('button', { name: 'Workspace' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await waitFor(() => expect(canvas.getByTestId('graph-node-count')).toHaveTextContent('0'));
    await expect(canvas.getByTestId('graph-edge-count')).toHaveTextContent('0');
    await expect(breadcrumb.getByText('No system')).toBeVisible();
    await useAppStore.getState().refresh();
    await expect(breadcrumb.getByText('No system')).toBeVisible();

    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await expect(canvas.getByText('ingest-source-v1:src_11111111111111111111111111111111')).toBeInTheDocument();
  },
};

export const CuratedSectionFollowsReviewsAndExportMode: Story = {
  // AC-0655 (SPEC-12 slice 3): the Workbench's curated section is re-read
  // when a review is recorded and when the export mode changes, with no
  // polling, and it shows the revision the bundle on screen carries.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    installFakeCore({ staged: [stagedFixture('proposal:pending')] });
    await useAppStore.getState().loadStagedProposals();
    await useAppStore.getState().reloadSpec();
    await userEvent.click(canvas.getByRole('button', { name: 'Spec Workbench' }));
    const section = within(await canvas.findByRole('region', { name: 'Curated items' }));
    const sameRevision = async () => {
      const state = useAppStore.getState();
      await expect(state.specCurated?.status).toBe('computed');
      await expect(section.getByTestId('curated-revision')).toHaveAttribute(
        'title',
        state.specBundle?.curated_basis?.revision,
      );
      return state.specBundle?.curated_basis?.revision;
    };
    // A pending proposal is not an accepted record.
    await expect(section.getByTestId('curated-empty')).toBeVisible();
    const before = await sameRevision();

    // A review is recorded: the views are reloaded, and the section with them.
    const pending = useAppStore.getState().stagedProposals[0];
    await expect(await useAppStore.getState().recordProposalDecision(pending, 'accepted')).toBe(true);
    await waitFor(() => expect(section.getByLabelText('Curated record counts')).toHaveTextContent('Unverified1'));
    await userEvent.click(section.getByText(/^Records and decisions: 0 curated items shown, 1 listed/));
    const listed = within(await section.findByRole('table', { name: 'State unverified (1)' }));
    await expect(listed.getByRole('row', { name: /proposal:pending/ })).toHaveTextContent(
      'The record predates captured evidence, so nothing ties its evidence to stored source bytes. (legacy_record)',
    );
    await expect(listed.getByText('Inferred (weak)')).toBeVisible();
    await expect(section.queryByTestId('curated-empty')).not.toBeInTheDocument();
    await expect(await sameRevision()).not.toBe(before);

    // The export mode changes: the section is read again for that mode.
    await userEvent.click(canvas.getByRole('button', { name: 'verified-only' }));
    await waitFor(() => expect(section.getByTestId('curated-omitted')).toHaveTextContent('Curated items left out: 0.'));
    await expect(useAppStore.getState().specCurated).toMatchObject({ export_mode: 'verified-only' });
    await sameRevision();
    // Listed records are the same in both modes.
    await expect(section.getByRole('table', { name: 'State unverified (1)' })).toBeVisible();
  },
};

export const SavedProposalReviewResumesAfterUiReset: Story = {
  // AC-0129: pending/reviewed history comes from durable host records, with explicit paging.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    installFakeCore({ staged: [
      stagedFixture('proposal:pending'), stagedFixture('proposal:accepted', 'accepted'),
      stagedFixture('proposal:older', 'rejected'),
    ], pageSize: 2 });
    useAppStore.setState({ escalation: null, stagedProposals: [], stagedNextCursor: null });
    await useAppStore.getState().loadStagedProposals();
    await useAppStore.getState().reloadSpec();
    await userEvent.click(canvas.getByRole('button', { name: 'Gaps & Drift' }));
    await userEvent.click(canvas.getByRole('tab', { name: 'Proposal history' }));
    await expect(canvas.getByText('Pending review')).toBeInTheDocument();
    // AC-0655: the accepted record shows the state the curated read gave it.
    await expect(canvas.getByText('Accepted · unverified')).toBeInTheDocument();
    await expect(canvas.queryByText('Saved annotation for proposal:older.')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Load more proposals' }));
    await waitFor(() => expect(canvas.getByText('Saved annotation for proposal:older.')).toBeInTheDocument());
    await expect(historyRequests).toEqual([
      { limit: 20, cursor: null }, { limit: 20, cursor: 'proposal:accepted' },
    ]);
    await expect(canvas.queryByRole('button', { name: 'Load more proposals' })).not.toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Review proposal' }));
    const dialog = within(canvas.getByRole('dialog'));
    await expect(dialog.getByText('Saved annotation for proposal:pending.')).toBeInTheDocument();
    await expect(dialog.getByText(/Source binding unverified:/)).toBeInTheDocument();
    await userEvent.click(dialog.getByRole('button', { name: 'Accept as InferredWeak' }));
    await waitFor(() => expect(dialog.getByTestId('decision-recorded')).toHaveTextContent(
      'State: unverified. The record predates captured evidence',
    ));
    await expect(reviewRequests).toEqual([{
      proposalId: 'proposal:pending', expectedRevision: 0, decision: 'accepted', note: null,
    }]);
    await userEvent.click(dialog.getByRole('button', { name: 'Close resolution strategy' }));

    // Lose all in-memory review history; the host still returns the reviewed record.
    useAppStore.setState({ stagedProposals: [], stagedNextCursor: null, escalation: null });
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh proposal history' }));
    await waitFor(() => expect(canvas.getAllByText('Accepted · unverified')).toHaveLength(2));
    await expect(canvas.queryByRole('button', { name: 'Review proposal' })).not.toBeInTheDocument();
    await expect(useAppStore.getState().stagedProposals[0].review_revision).toBe(1);
  },
};

export const StaleProposalReviewIsNotRecorded: Story = {
  // AC-0129: compare-and-set rejection must leave the proposal reviewable, never accepted locally.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    installFakeCore({ staged: [stagedFixture()], reviewFailure: 'stale' });
    await useAppStore.getState().loadStagedProposals();
    useAppStore.getState().openStagedProposal(useAppStore.getState().stagedProposals[0]);
    const dialog = within(await canvas.findByRole('dialog'));
    await userEvent.click(dialog.getByRole('button', { name: 'Accept as InferredWeak' }));
    await waitFor(() => expect(dialog.getByRole('alert')).toHaveTextContent('stale review revision'));
    await expect(dialog.queryByTestId('decision-recorded')).not.toBeInTheDocument();
    await expect(useAppStore.getState().stagedProposals[0].review_decision).toBeNull();
    await expect(dialog.getByRole('button', { name: 'Accept as InferredWeak' })).toBeEnabled();
  },
};

export const MissingReviewReceiptIsNotSuccess: Story = {
  // AC-0129: a null invoke response is not evidence that a review was persisted.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    installFakeCore({ staged: [stagedFixture()], reviewFailure: 'null' });
    await useAppStore.getState().loadStagedProposals();
    useAppStore.getState().openStagedProposal(useAppStore.getState().stagedProposals[0]);
    const dialog = within(await canvas.findByRole('dialog'));
    await userEvent.click(dialog.getByRole('button', { name: 'Reject' }));
    await waitFor(() => expect(dialog.getByRole('alert')).toHaveTextContent('No review was recorded'));
    await expect(dialog.queryByTestId('decision-recorded')).not.toBeInTheDocument();
    await expect(useAppStore.getState().stagedProposals[0].review_revision).toBe(0);
  },
};

export const UnstagedBrokerBodyCannotBeReviewed: Story = {
  // AC-0129: an unstaged broker body has no host ID and must not expose acceptance actions.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    installFakeCore({ unstagedResult: true });
    await useAppStore.getState().openResolution('gap:sync');
    const dialog = within(await canvas.findByRole('dialog'));
    await userEvent.click(dialog.getByRole('button', { name: 'Run locally' }));
    await waitFor(() => expect(dialog.getByRole('alert')).toHaveTextContent('did not return a saved proposal'));
    await expect(dialog.queryByTestId('proposal-card')).not.toBeInTheDocument();
    await expect(dialog.queryByRole('button', { name: /Accept as/ })).not.toBeInTheDocument();
  },
};

export const OlderHistoryResponsePreservesNewReview: Story = {
  // AC-0129: an in-flight page cannot roll back a newer host review receipt.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    let releaseHistory!: () => void;
    const historyGate = new Promise<void>((resolve) => { releaseHistory = resolve; });
    const proposal = stagedFixture();
    installFakeCore({ staged: [proposal], historyGate });
    useAppStore.setState({ stagedProposals: [proposal] });
    const pendingRead = useAppStore.getState().loadStagedProposals();
    await waitFor(() => expect(historyRequests).toHaveLength(1));
    useAppStore.getState().openStagedProposal(proposal);
    const dialog = within(await canvas.findByRole('dialog'));
    await userEvent.click(dialog.getByRole('button', { name: 'Accept as InferredWeak' }));
    await waitFor(() => expect(dialog.getByTestId('decision-recorded')).toBeInTheDocument());
    releaseHistory();
    await pendingRead;
    await expect(useAppStore.getState().stagedProposals[0].review_revision).toBe(1);
    await expect(useAppStore.getState().stagedProposals[0].review_decision).toBe('accepted');
  },
};

export const CompletedRunPreservesOpenedHistoryRecord: Story = {
  // AC-0129: a new stage enters history without replacing another opened record of the same gap.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    let releaseRun!: () => void;
    const runGate = new Promise<void>((resolve) => { releaseRun = resolve; });
    const earlier = stagedFixture('proposal:earlier', 'rejected');
    installFakeCore({ staged: [earlier], runGate });
    await useAppStore.getState().openResolution(earlier.gap_id);
    const pendingRun = useAppStore.getState().runStrategy('local-slm');
    useAppStore.getState().openStagedProposal(earlier);
    releaseRun();
    await pendingRun;
    const dialog = within(await canvas.findByRole('dialog'));
    await expect(dialog.getByText(earlier.annotation)).toBeInTheDocument();
    await expect(dialog.getByTestId('decision-recorded')).toHaveTextContent('rejected');
    await expect(useAppStore.getState().escalation?.proposal?.proposal_id).toBe(earlier.proposal_id);
    await expect(useAppStore.getState().stagedProposals.some((proposal) =>
      proposal.proposal_id === 'proposal:host-result')).toBe(true);
  },
};

export const DelayedReviewReceiptPreservesNewestOpenedRevision: Story = {
  // AC-0129: an older review receipt cannot replace a newer decision loaded from the host.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await waitFor(() => expect(useAppStore.getState().stagedLoading).toBe(false));
    let releaseReview!: () => void;
    const reviewGate = new Promise<void>((resolve) => { releaseReview = resolve; });
    const pending = stagedFixture();
    installFakeCore({ staged: [pending], reviewGate });
    useAppStore.getState().openStagedProposal(pending);
    const delayedReview = useAppStore.getState().decideProposal('accepted');
    await waitFor(() => expect(reviewRequests).toHaveLength(1));

    // A different reviewer records revision 2 before revision 1 reaches this UI.
    const newer = { ...stagedFixture(pending.proposal_id, 'rejected'), review_revision: 2 };
    installFakeCore({ staged: [newer] });
    await useAppStore.getState().loadStagedProposals();
    useAppStore.getState().openStagedProposal(useAppStore.getState().stagedProposals[0]);
    releaseReview();
    await delayedReview;
    const dialog = within(await canvas.findByRole('dialog'));
    await expect(dialog.getByTestId('decision-recorded')).toHaveTextContent('Decision recorded: rejected');
    await expect(dialog.getByTestId('decision-recorded')).not.toHaveTextContent('Decision recorded: accepted');
    await expect(useAppStore.getState().escalation?.proposal?.review_revision).toBe(2);
    await expect(useAppStore.getState().stagedProposals[0].review_revision).toBe(2);
    await expect(useAppStore.getState().stagedProposals[0].review_decision).toBe('rejected');
  },
};

export const InvestigationWorkspaceCloudHistoryAndJobs: Story = {
  // AC-0186/0189/0190: actual App/store/command wiring with scripted host responses.
  // This fixture is not a real-provider execution or the independent H4 manual gate.
  beforeEach: () => { installFakeCore({ investigationFlow: true }); },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await userEvent.click(canvas.getByRole('button', { name: 'Open investigations' }));
    await waitFor(() => expect(canvas.getByRole('radio', { name: /Domain analyst/ })).toBeVisible());
    await userEvent.selectOptions(canvas.getByLabelText('Provider'), 'cloud');
    await userEvent.type(canvas.getByLabelText('Question'), 'Which stock behavior has evidence, and what remains unknown?');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await waitFor(() => expect(canvas.getByRole('button', { name: 'Review next cloud action' })).toBeEnabled());
    await expect(investigationRequests.filter((request) => request.command === 'start_investigation')).toHaveLength(1);
    await userEvent.click(canvas.getByRole('button', { name: 'Review next cloud action' }));
    let dialog = within(canvas.getByRole('dialog', { name: 'Review exact model payload' }));
    await expect(dialog.getByText('payload:inv-app-script:step-1')).toBeVisible();
    await userEvent.click(dialog.getByRole('button', { name: 'Review later' }));
    await expect(canvas.queryByRole('dialog', { name: 'Review exact model payload' })).not.toBeInTheDocument();
    await expect(investigationRequests.filter((request) => request.command === 'approve_investigation_step')).toHaveLength(0);
    await userEvent.click(canvas.getByRole('button', { name: 'Review next cloud action' }));
    dialog = within(canvas.getByRole('dialog', { name: 'Review exact model payload' }));
    await userEvent.click(dialog.getByRole('button', { name: 'Allow this action once' }));
    await waitFor(() => expect(useInvestigationStore.getState().consent?.step_id).toBe('step-2'));
    await expect(canvas.queryByRole('dialog', { name: 'Review exact model payload' })).not.toBeInTheDocument();
    await expect(canvas.getByText('Source validation budget reserved')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Review next cloud action' }));
    dialog = within(canvas.getByRole('dialog', { name: 'Review exact model payload' }));
    await expect(dialog.getByText('payload:inv-app-script:step-2')).toBeVisible();
    await userEvent.click(dialog.getByRole('button', { name: 'Allow this action once' }));
    await waitFor(() => expect(canvas.getByText('A local stock guard is present')).toBeVisible());
    await expect(investigationRequests.filter((request) => request.command === 'approve_investigation_step').map((request) =>
      (request.args as { stepId: string }).stepId)).toEqual(['step-1', 'step-2']);
    await userEvent.click(canvas.getByRole('button', { name: 'Inspect citation C1' }));
    await waitFor(() => expect(canvas.getByText('ORIGINAL RETAINED APP FIXTURE SOURCE')).toBeVisible());
    await expect(investigationRequests.at(-1)).toEqual({ command: 'read_investigation_citation',
      args: { investigationId: 'inv-app-script', citationId: 'C1' } });
    const savedResult = JSON.stringify(useInvestigationStore.getState().result);

    // Refresh Jobs from the host, then follow its opaque association to the same task.
    await useAppStore.getState().refresh();
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await userEvent.click(canvas.getByRole('button', { name: 'View investigation' }));
    await waitFor(() => expect(canvas.getByText('A local stock guard is present')).toBeVisible());
    await expect(useInvestigationStore.getState().selectedId).toBe('inv-app-script');
    await expect(JSON.stringify(useInvestigationStore.getState().result)).toBe(savedResult);
    await expect(canvas.queryByRole('button', { name: /^(Retry|Resume)$/ })).not.toBeInTheDocument();
    await expect(canvas.getByRole('button', { name: 'Workspace' })).toHaveAttribute('aria-current', 'page');
  },
};

export const InvestigationResultCommitStraddleIsRejected: Story = {
  // AC-0204: a refresh whose result read misses the commit its detail already
  // reports is rejected, not shown as a completed task without findings; the
  // next refresh reads the consistent pair.
  beforeEach: () => { installFakeCore({ investigationFlow: true, straddleResultCommit: true }); },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(useAppStore.getState().backend).toBe('up'));
    await userEvent.click(canvas.getByRole('button', { name: 'Open investigations' }));
    await waitFor(() => expect(canvas.getByRole('radio', { name: /Domain analyst/ })).toBeVisible());
    await userEvent.selectOptions(canvas.getByLabelText('Provider'), 'cloud');
    await userEvent.type(canvas.getByLabelText('Question'), 'Which stock behavior has evidence?');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    for (const step of ['step-1', 'step-2']) {
      await waitFor(() => expect(canvas.getByRole('button', { name: 'Review next cloud action' })).toBeEnabled());
      await waitFor(() => expect(useInvestigationStore.getState().consent?.step_id).toBe(step));
      await userEvent.click(canvas.getByRole('button', { name: 'Review next cloud action' }));
      const dialog = within(canvas.getByRole('dialog', { name: 'Review exact model payload' }));
      await userEvent.click(dialog.getByRole('button', { name: 'Allow this action once' }));
    }
    await waitFor(() => expect(canvas.getByText(/could not be refreshed consistently/)).toBeVisible());
    await expect(useInvestigationStore.getState().result).toBeNull();
    // The approval's completed summary is not merged ahead of a consistent read.
    await expect(useInvestigationStore.getState().detail?.status).not.toBe('completed');
    await expect(useInvestigationStore.getState().detail?.has_result).toBe(false);
    await expect(canvas.queryByText('A local stock guard is present')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh selected investigation' }));
    await waitFor(() => expect(canvas.getByText('A local stock guard is present')).toBeVisible());
    await expect(useInvestigationStore.getState().detail?.status).toBe('completed');
    await expect(canvas.queryByText(/could not be refreshed consistently/)).not.toBeInTheDocument();
  },
};

export const CapturedRangeBecomesExplicitInvestigationRequirement: Story = {
  // AC-0261: real App/store route; viewer read never creates an agent obligation.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const endpoints = canvas.getByRole('heading', { name: 'Endpoints' }).closest('section') as HTMLElement;
    installFakeCore({ capturedDescriptions: true, inspectionSelection: true, investigationFlow: true });
    await userEvent.click(await within(endpoints).findByRole('button', { name: /GET \/users/ }));
    await userEvent.selectOptions(await canvas.findByLabelText('Captured cited range'), '0');
    await waitFor(() => expect(usePrimarySourceStore.getState().loading).toBe(false));
    await expect(investigationRequests).toHaveLength(0);
    await expect(useInvestigationStore.getState().draftInspectionRequirements).toHaveLength(0);
    await userEvent.click(canvas.getByRole('button', { name: 'Require this source in an investigation' }));
    await expect(await canvas.findByText('local/src/app.ts')).toBeVisible();
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@3');
    await expect(investigationRequests[0]).toEqual({ command: 'resolve_investigation_inspection_target', args: {
      fact: { kind: 'node', id: FAKE_ENDPOINT.id }, expectedGraphSnapshotId: 'snapshot:original',
      expectedReceiptId: 'receipt:selection-1', inventoryIndex: 0,
    } });
    const target = useInvestigationStore.getState().draftInspectionRequirements[0];
    await expect(target.index).toBe(2);
    await expect(target.receipt_inventory_index).toBe(0);
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'domain-analyst@5');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect this exact captured source.');
    await userEvent.click(canvas.getByRole('button', { name: 'Start investigation' }));
    await waitFor(() => expect(investigationRequests.some((item) => item.command === 'start_investigation')).toBe(true));
    const sent = (investigationRequests.find((item) => item.command === 'start_investigation')?.args as { request: StartInvestigationRequest }).request;
    await expect(sent.schema_version).toBe(2);
    await expect(sent.specialist_id).toBe('domain-analyst@5');
    await expect(sent.inspection_requirements).toEqual([target]);
    await expect(sent.expected_graph_revision).toBe(target.graph_snapshot_id);
    await expect(typeof sent.request_nonce).toBe('string');
  },
};


export const InspectionFollowUpSurvivesSourceSelectionNavigation: Story = {
  // AC-0261: actual App unmount/remount retains explicit composer intent, never old requirements.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const parent = inspectionDetail('inv-parent', {
      specialist_id: 'domain-analyst@5', specialist: inspectionCatalog.specialists.find((item) => item.id === 'domain-analyst@5')!,
      status: 'interrupted', has_result: false, revision: 0, last_event_sequence: 0,
      inspection_status: 'not_assessed', inspection_assessment: undefined,
      inspection_requirements: [inspectionTarget], inspection_requirement_count: 1,
      actions: { can_cancel: false, can_follow_up: true },
    });
    installFakeCore({ existingInvestigation: parent, inspectionSelection: true, capturedDescriptions: true, investigationFlow: true });
    await userEvent.click(canvas.getByRole('button', { name: 'Open investigations' }));
    await useInvestigationStore.getState().open(parent.investigation_id);
    await userEvent.click(await canvas.findByRole('button', { name: 'Ask a follow-up' }));
    await expect(useInvestigationStore.getState().draftInspectionRequirements).toHaveLength(0);
    await userEvent.selectOptions(canvas.getByLabelText('Specialist version'), 'evidence-auditor@5');
    await userEvent.selectOptions(canvas.getByLabelText('Provider'), 'cloud');
    await userEvent.selectOptions(canvas.getByLabelText('Scope'), 'neighborhood');
    await expect(useInvestigationStore.getState().composer.scopeType).toBe('neighborhood');
    const anchor = await canvas.findByLabelText(/^Anchor fact ID/);
    await userEvent.clear(anchor);
    await userEvent.type(anchor, FAKE_ENDPOINT.id);
    await userEvent.selectOptions(canvas.getByLabelText('Hops'), '2');
    const question = 'Inspect this newly selected source in the follow-up.';
    await userEvent.type(canvas.getByLabelText('Follow-up question'), question);
    await userEvent.click(canvas.getByRole('button', { name: 'Workspace' }));
    const endpoints = canvas.getByRole('heading', { name: 'Endpoints' }).closest('section') as HTMLElement;
    await userEvent.click(await within(endpoints).findByRole('button', { name: /GET \/users/ }));
    await userEvent.selectOptions(await canvas.findByLabelText('Captured cited range'), '0');
    await waitFor(() => expect(usePrimarySourceStore.getState().loading).toBe(false));
    await userEvent.click(canvas.getByRole('button', { name: 'Require this source in an investigation' }));
    await expect(await canvas.findByLabelText('Follow-up question')).toHaveValue(question);
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('evidence-auditor@5');
    await expect(canvas.getByLabelText('Provider')).toHaveValue('cloud');
    await expect(canvas.getByLabelText(/^Anchor fact ID/)).toHaveValue(FAKE_ENDPOINT.id);
    await expect(canvas.getByLabelText('Hops')).toHaveValue('2');
    const requirements = structuredClone(useInvestigationStore.getState().draftInspectionRequirements);
    await expect(requirements).toHaveLength(1);
    await expect(requirements[0].target_id).not.toBe(inspectionTarget.target_id);
    await userEvent.click(canvas.getByRole('button', { name: 'Start follow-up' }));
    await waitFor(() => expect(investigationRequests.some((item) => item.command === 'start_investigation')).toBe(true));
    const request = (investigationRequests.find((item) => item.command === 'start_investigation')!.args as { request: StartInvestigationRequest }).request;
    await expect(request).toMatchObject({ schema_version: 2, question, specialist_id: 'evidence-auditor@5', provider_mode: 'cloud',
      scope: { type: 'neighborhood', anchor: FAKE_ENDPOINT.id, hops: 2 },
      parent_id: parent.investigation_id, conversation_id: parent.conversation_id, inspection_requirements: requirements });
    await waitFor(() => expect(useInvestigationStore.getState().pendingStart).toBeNull());
    await expect(useInvestigationStore.getState().composer.parent).toBeNull();
    await expect(useInvestigationStore.getState().composer.question).toBe('');
    await expect(useInvestigationStore.getState().draftInspectionRequirements).toHaveLength(0);
  },
};


export const NextPromptCapturedSourceBecomesCompletionObligation: Story = {
  // AC-0287: actual App navigation chooses the completion resolver for @10.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    installFakeCore({ capturedDescriptions: true, admissionSelection: true });
    await userEvent.click(canvas.getByRole('button', { name: 'Open investigations' }));
    await useInvestigationStore.getState().loadCatalog();
    await userEvent.selectOptions(await canvas.findByLabelText('Specialist version'), 'domain-analyst@10');
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@10');
    await userEvent.click(canvas.getByRole('button', { name: 'Workspace' }));
    const endpoints = canvas.getByRole('heading', { name: 'Endpoints' }).closest('section') as HTMLElement;
    await userEvent.click(await within(endpoints).findByRole('button', { name: /GET \/users/ }));
    await userEvent.selectOptions(await canvas.findByLabelText('Captured cited range'), '0');
    await waitFor(() => expect(usePrimarySourceStore.getState().loading).toBe(false));
    await userEvent.click(canvas.getByRole('button', { name: 'Require this source in an investigation' }));
    await waitFor(() => expect(useInvestigationStore.getState().draftObligations).toHaveLength(1));
    const state = useInvestigationStore.getState();
    await expect(state.draftInspectionRequirements).toHaveLength(0);
    await expect(state.draftObligations[0]).toMatchObject({ kind: 'source', target: {
      fact: { kind: 'node', id: FAKE_ENDPOINT.id }, graph_snapshot_id: completionChoices.graph_snapshot_id } });
    await expect(investigationRequests.map((item) => item.command)).toEqual([
      'resolve_investigation_inspection_target', 'resolve_investigation_source_obligation' ]);
    await expect(await canvas.findByLabelText('Question')).toBeVisible();
    await expect(canvas.getByLabelText('Specialist version')).toHaveValue('domain-analyst@10');
    await userEvent.type(canvas.getByLabelText('Question'), 'Inspect this newly selected completion source.');
    await expect(canvas.getByRole('button', { name: 'Start investigation' })).toBeEnabled();
  },
};

// AC-0398: actual parent capture, inline cancellation, exact retry and serialized child landing.
// AC-0413: pane attribution does not remount the reply or mutate its durable request.
export const InlineReplyUsesItsValidatedPane: Story = {
  beforeEach: () => {
    const id = 'inv-inline-parent'; const parent = investigationDetail(id, { question: 'Parent for inline reply' });
    installFakeCore({ investigationFlow: true, startUncertainOnce: true, existingInvestigation: parent,
      existingInvestigationEvents: investigationEvents(id), savedNoticeChat: savedNoticeChat(id, parent.question),
      savedNoticeChatFirstReadPartial: false });
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const composer = { ...useInvestigationStore.getState().composer, question: 'Preserve the full composer' };
    useInvestigationStore.setState({ composer });
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Parent for inline reply/ }));
    let pane = within(await canvas.findByRole('region', { name: 'Saved chat inv-inline-parent' }));
    await userEvent.click(pane.getByRole('button', { name: 'Reply here' }));
    let reply = within(await pane.findByRole('region', { name: 'Reply to inv-inline-parent' }));
    await expect(canvas.queryByRole('dialog')).not.toBeInTheDocument();
    await userEvent.type(reply.getByLabelText('Question'), 'This canceled reply must not dispatch.');
    const search = canvas.getByLabelText('Search saved questions, specialist names or IDs, and answers');
    search.focus();
    fireEvent.keyDown(search, { key: 'Escape', code: 'Escape', bubbles: true });
    await expect(reply.getByLabelText('Question')).toHaveValue('This canceled reply must not dispatch.');
    await expect(reply.getByRole('button', { name: 'Send reply task' })).toBeInTheDocument();
    reply.getByLabelText('Question').focus();
    fireEvent.keyDown(reply.getByLabelText('Question'), { key: 'Escape', code: 'Escape', bubbles: true });
    await waitFor(() => expect(canvas.queryByRole('region', { name: 'Reply to inv-inline-parent' })).not.toBeInTheDocument());
    await expect(investigationRequests.filter((request) => request.command === 'start_investigation')).toHaveLength(0);
    await userEvent.click(pane.getByRole('button', { name: 'Reply here' }));
    reply = within(await pane.findByRole('region', { name: 'Reply to inv-inline-parent' }));
    const canceledStart = reply.getByRole('button', { name: 'Send reply task' });
    fireEvent.click(pane.getByRole('button', { name: 'Close inv-inline-parent' }));
    fireEvent.click(canceledStart);
    await expect(investigationRequests.filter((request) => request.command === 'start_investigation')).toHaveLength(0);
    await userEvent.click(canvas.getByRole('button', { name: /Parent for inline reply/ }));
    pane = within(await canvas.findByRole('region', { name: 'Saved chat inv-inline-parent' }));
    await userEvent.click(pane.getByRole('button', { name: 'Reply here' }));
    reply = within(await pane.findByRole('region', { name: 'Reply to inv-inline-parent' }));
    await expect(reply.getByLabelText('Question')).toHaveFocus();
    const replyHost = canvasElement.querySelector('[data-reply-task="inv-inline-parent"]');
    const attribution = pane.getByRole('group', { name: 'Saved pane attribution' }).textContent;
    await useInvestigationStore.getState().open('unrelated-selection');
    expect(pane.getByRole('group', { name: 'Saved pane attribution' }).textContent).toBe(attribution);
    expect(canvasElement.querySelector('[data-reply-task="inv-inline-parent"]')).toBe(replyHost);
    await userEvent.type(reply.getByLabelText('Question'), 'Continue from the saved parent result.');
    await expect(reply.getByLabelText('Question')).toHaveValue('Continue from the saved parent result.');
    fireEvent.keyDown(reply.getByLabelText('Question'), { key: 'Enter', ctrlKey: true, bubbles: true });
    await waitFor(() => expect(useInvestigationStore.getState().pendingStart).not.toBeNull());
    const pending = structuredClone(useInvestigationStore.getState().pendingStart!);
    const reads = investigationRequests.length;
    await expect(pane.getByRole('group', { name: 'Saved pane attribution' })).toHaveTextContent('Saved request · Local');
    expect(investigationRequests).toHaveLength(reads);
    expect(canvasElement.querySelector('[data-reply-task="inv-inline-parent"]')).toBe(replyHost);
    expect(pending.question).toBe('Continue from the saved parent result.');
    await expect(reply.getByRole('button', { name: 'Retry same request' })).toBeEnabled();
    fireEvent.keyDown(reply.getByRole('button', { name: 'Retry same request' }), { key: ' ', code: 'Space', metaKey: true, bubbles: true });
    const dialog = within(await canvas.findByRole('dialog', { name: 'Quick investigation dispatch' }));
    await expect(canvas.queryByRole('region', { name: 'Reply to inv-inline-parent' })).not.toBeInTheDocument();
    await expect(canvas.getAllByRole('dialog')).toHaveLength(1);
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
    await userEvent.click(dialog.getByRole('button', { name: 'Retry same request' }));
    const child = within(await canvas.findByRole('region', { name: 'Saved chat inv-app-follow-up' }));
    const requests = investigationRequests.filter((item) => item.command === 'start_investigation');
    expect(requests).toHaveLength(2); expect(requests[1].args).toEqual(requests[0].args);
    expect(pending).toMatchObject({ parent_id: 'inv-inline-parent', conversation_id: 'conversation-inv-inline-parent',
      specialist_id: 'domain-analyst@3', provider_mode: 'local', scope: { type: 'neighborhood', anchor: 'rule:stock', hops: 1 } });
    await expect(child.getByText(/Follow-up to inv-inline-parent/)).toBeVisible();
    await expect(child.getByRole('listitem', { name: 'User question' })).toHaveTextContent(pending.question);
    expect(investigationRequests.filter((item) => item.command === 'get_investigation_chat' &&
      (item.args as { investigationId: string }).investigationId === 'inv-app-follow-up')).toHaveLength(1);
    await useInvestigationStore.getState().approve(); await useInvestigationStore.getState().approve();
    useAgentChatDockStore.getState().open('inv-app-follow-up');
    await expect(await child.findByText('The inspected callable has a guarded exit. Its broader inventory policy remains unresolved.')).toBeVisible();
    await expect(useAgentChatDockStore.getState().docks.length).toBeLessThanOrEqual(4);
    await expect(useInvestigationStore.getState().composer).toEqual(composer);
  },
};

export const InlineReplySupersededCaptureSurvivesOldClose: Story = {
  beforeEach: () => {
    const a = investigationDetail('inline-a', { question: 'Inline parent A' });
    const b = investigationDetail('inline-b', { question: 'Inline parent B' });
    installFakeCore({ existingInvestigation: a, existingInvestigationEvents: investigationEvents('inline-a'), additionalSavedParent: b,
      savedNoticeChat: savedNoticeChat('inline-a', a.question), savedNoticeChatFirstReadPartial: false,
      deferSavedNoticeChatRead: true, deferSavedNoticeChatAfterReads: 3 });
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    for (const id of ['A', 'B']) {
      await userEvent.click(canvas.getByRole('button', { name: new RegExp(`Inline parent ${id}`) }));
      await canvas.findByRole('region', { name: `Saved chat inline-${id.toLowerCase()}` });
    }
    const a = within(canvas.getByRole('region', { name: 'Saved chat inline-a' }));
    const b = within(canvas.getByRole('region', { name: 'Saved chat inline-b' }));
    await userEvent.click(a.getByRole('button', { name: 'Reply here' }));
    await a.findByRole('region', { name: 'Reply to inline-a' });
    await userEvent.click(b.getByRole('button', { name: 'Reply here' }));
    await expect(canvas.getByText('Refreshing parent task inline-b…')).toBeVisible();
    await waitFor(() => expect(canvas.queryByRole('region', { name: 'Reply to inline-a' })).not.toBeInTheDocument());
    releaseSavedNoticeChatRead?.();
    await b.findByLabelText('Question');
    await waitFor(() => expect(b.getByLabelText('Question')).toHaveFocus());
    const oldClose = b.getByRole('button', { name: 'Close inline reply' });
    fireEvent.click(b.getByRole('button', { name: 'Reply here' }));
    fireEvent.click(oldClose);
    await expect(canvas.getByText('Refreshing parent task inline-b…')).toBeVisible();
    await waitFor(() => expect(b.queryByRole('region', { name: 'Reply to inline-b' })).not.toBeInTheDocument());
    releaseSavedNoticeChatRead?.();
    await expect(await b.findByRole('region', { name: 'Reply to inline-b' })).toBeVisible();
    await waitFor(() => expect(b.getByLabelText('Question')).toHaveFocus());
    await expect(investigationRequests.filter((request) => request.command === 'start_investigation')).toHaveLength(0);
  },
};

// AC-0400: adding an unloaded ancestor regroups real docks without remounting the active reply portal.
export const InlineReplySurvivesConversationGrouping: Story = {
  beforeEach: () => {
    const child = investigationDetail('branch-child', { question: 'Branch child to reply to',
      conversation_id: 'saved-branch', parent_id: 'branch-root' });
    const parent = investigationDetail('branch-root', { question: 'Unloaded branch root', conversation_id: 'saved-branch' });
    const unrelated = investigationDetail('unrelated-branch', { question: 'Unrelated saved exchange' });
    installFakeCore({ investigationFlow: true, existingInvestigation: child, existingInvestigationEvents: investigationEvents(child.investigation_id),
      savedNoticeChat: savedNoticeChat(child.investigation_id, child.question), savedNoticeChatFirstReadPartial: false,
      additionalSavedParent: parent, additionalSavedExchanges: [unrelated] });
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Branch child to reply to/ }));
    const pane = canvas.getByRole('region', { name: 'Saved chat branch-child' });
    await userEvent.click(canvas.getByRole('button', { name: /Unrelated saved exchange/ }));
    await canvas.findByRole('region', { name: 'Saved chat unrelated-branch' });
    await userEvent.click(within(pane).getByRole('button', { name: 'Reply here' }));
    const reply = within(await within(pane).findByRole('region', { name: 'Reply to branch-child' }));
    const host = pane.querySelector('[data-reply-task="branch-child"]');
    const question = reply.getByLabelText('Question');
    await userEvent.type(question, 'Continue this exact saved branch.');
    await userEvent.click(within(pane).getByRole('button', { name: 'Open unloaded parent branch-root' }));
    await canvas.findByRole('region', { name: 'Saved chat branch-root' });
    expect(canvas.getAllByRole('region', { name: /^Saved chat / }).map((node) => node.getAttribute('aria-label')))
      .toEqual(['Saved chat branch-child', 'Saved chat branch-root', 'Saved chat unrelated-branch']);
    expect(pane.querySelector('[data-reply-task="branch-child"]')).toBe(host);
    expect(host?.isConnected).toBe(true);
    expect(reply.getByLabelText('Question')).toBe(question);
    await expect(question).toHaveValue('Continue this exact saved branch.');
    await expect(canvas.getAllByRole('region', { name: 'Reply to branch-child' })).toHaveLength(1);
    await userEvent.click(reply.getByRole('button', { name: 'Send reply task' }));
    await canvas.findByRole('region', { name: 'Saved chat inv-app-follow-up' });
    const requests = investigationRequests.filter((item) => item.command === 'start_investigation');
    expect(requests).toHaveLength(1);
    expect((requests[0].args as { request: StartInvestigationRequest }).request).toMatchObject({
      parent_id: 'branch-child', conversation_id: 'saved-branch', question: 'Continue this exact saved branch.' });
  },
};

// AC-0409: the real shell keeps one inline composer across search and modal handoff.
export const ChatSearchPreservesInlineReply: Story = {
  ...InlineReplyUsesItsValidatedPane,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Parent for inline reply/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat inv-inline-parent' }));
    await userEvent.click(pane.getByRole('button', { name: 'Reply here' }));
    const reply = within(await pane.findByRole('region', { name: 'Reply to inv-inline-parent' }));
    const question = reply.getByLabelText('Question');
    const host = canvasElement.querySelector('[data-reply-task="inv-inline-parent"]');
    await userEvent.type(question, 'Keep this exact inline parent and draft.');
    const openSearch = async (target = question) => {
      target.focus(); fireEvent.keyDown(target, { key: 'f', ctrlKey: true, bubbles: true });
      return within(await canvas.findByRole('dialog', { name: 'Search open and saved chats' }));
    };
    let search = await openSearch();
    await waitFor(() => expect(search.getByRole('searchbox')).toHaveFocus());
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(question).toHaveFocus());
    await expect(canvasElement.querySelector('[data-reply-task="inv-inline-parent"]')).toBe(host);
    await expect(question).toHaveValue('Keep this exact inline parent and draft.');
    search = await openSearch();
    fireEvent.keyDown(search.getByRole('searchbox'), { key: 'k', ctrlKey: true, bubbles: true });
    const command = within(await canvas.findByRole('dialog', { name: 'Command palette' }));
    await waitFor(() => expect(command.getByRole('listbox')).toHaveFocus());
    await expect(canvas.getAllByRole('dialog')).toHaveLength(1);
    fireEvent.keyDown(command.getByRole('listbox'), { key: 'f', ctrlKey: true, bubbles: true });
    await expect(canvas.queryByRole('dialog', { name: 'Search open and saved chats' })).not.toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    await expect(question).toHaveValue('Keep this exact inline parent and draft.');
    await expect(investigationRequests.filter((item) => item.command === 'start_investigation')).toHaveLength(0);
    await userEvent.click(reply.getByRole('button', { name: 'Send reply task' }));
    await waitFor(() => expect(useInvestigationStore.getState().pendingStart).not.toBeNull());
    const pending = structuredClone(useInvestigationStore.getState().pendingStart);
    await expect(pending).toMatchObject({ parent_id: 'inv-inline-parent', question: 'Keep this exact inline parent and draft.' });
    await openSearch(reply.getByRole('button', { name: 'Retry same request' }));
    await userEvent.keyboard('{Escape}');
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
    await expect(canvasElement.querySelector('[data-reply-task="inv-inline-parent"]')).toBe(host);
    await expect(investigationRequests.filter((item) => item.command === 'start_investigation')).toHaveLength(1);
    await userEvent.click(canvas.getByRole('button', { name: 'Workspace' }));
    fireEvent.keyDown(canvasElement, { key: 'f', ctrlKey: true, bubbles: true });
    await expect(canvas.queryByRole('dialog', { name: 'Search open and saved chats' })).not.toBeInTheDocument();
  },
};

// AC-0411: presentation changes never reparent or clear the one real reply composer.
export const PaneLayoutPreservesInlineReply: Story = {
  ...InlineReplyUsesItsValidatedPane,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Parent for inline reply/ }));
    const pane = canvas.getByRole('region', { name: 'Saved chat inv-inline-parent' });
    await userEvent.click(within(pane).getByRole('button', { name: 'Reply here' }));
    const reply = within(await within(pane).findByRole('region', { name: 'Reply to inv-inline-parent' }));
    const question = reply.getByLabelText('Question');
    const host = pane.querySelector('[data-reply-task="inv-inline-parent"]');
    // AC-0416: the existing direct-pane reply host is outside the independent scroller.
    const reading = within(pane).getByRole('region', { name: 'Saved messages inv-inline-parent' });
    expect(host?.parentElement).toBe(pane); expect(reading.contains(host)).toBe(false);
    const requestsBeforeScroll = [...investigationRequests];
    expect(reading.scrollHeight).toBeGreaterThan(reading.clientHeight);
    reading.scrollTop = 75; expect(reading.scrollTop).toBe(75);
    expect(investigationRequests).toEqual(requestsBeforeScroll);
    await userEvent.type(question, 'Preserve this parent across pane layouts.');
    await waitFor(() => expect(reply.getByRole('button', { name: 'Send reply task' })).toBeEnabled());
    const requestsBefore = [...investigationRequests];
    for (const count of [2, 3, 4, 1]) {
      await userEvent.click(canvas.getByRole('button', { name: `${count} pane ${count === 1 ? 'slot' : 'slots'}` }));
      if (count === 3) for (const position of ['Top', 'Bottom']) await userEvent.click(canvas.getByRole('button', { name: position }));
      expect(pane.querySelector('[data-reply-task="inv-inline-parent"]')).toBe(host);
      expect(host?.isConnected).toBe(true); expect(reply.getByLabelText('Question')).toBe(question);
      expect(within(pane).getByRole('region', { name: 'Saved messages inv-inline-parent' })).toBe(reading);
      reading.scrollTop = 50; expect(reading.scrollTop).toBe(50);
      await expect(question).toHaveValue('Preserve this parent across pane layouts.');
    }
    expect(investigationRequests).toEqual(requestsBefore);
    await userEvent.click(reply.getByRole('button', { name: 'Send reply task' }));
    await waitFor(() => expect(useInvestigationStore.getState().pendingStart).not.toBeNull());
    const pending = structuredClone(useInvestigationStore.getState().pendingStart);
    expect(pending).toMatchObject({ parent_id: 'inv-inline-parent', conversation_id: 'conversation-inv-inline-parent',
      question: 'Preserve this parent across pane layouts.' });
    const afterSubmit = [...investigationRequests];
    reading.scrollTop = 25; expect(reading.scrollTop).toBe(25);
    expect(host?.parentElement).toBe(pane);
    await userEvent.click(canvas.getByRole('button', { name: '3 pane slots' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Top' }));
    await expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
    expect(investigationRequests).toEqual(afterSubmit);
    expect(pane.querySelector('[data-reply-task="inv-inline-parent"]')).toBe(host);
    expect(host?.isConnected).toBe(true);
    await expect(reply.getByRole('button', { name: 'Retry same request' })).toBeEnabled();
  },
};

// AC-0422: the real shell preserves inline state and ignores a departed view's reply.
export const LocalCheckPreservesInlineReply: Story = {
  ...InlineReplyUsesItsValidatedPane,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    await userEvent.click(await canvas.findByRole('button', { name: /Parent for inline reply/ }));
    const pane = within(await canvas.findByRole('region', { name: 'Saved chat inv-inline-parent' }));
    await userEvent.click(pane.getByRole('button', { name: 'Reply here' }));
    const reply = within(await pane.findByRole('region', { name: 'Reply to inv-inline-parent' }));
    await userEvent.type(reply.getByLabelText('Question'), 'Retain this parent and draft.');
    const host = canvasElement.querySelector('[data-reply-task="inv-inline-parent"]');
    const pending = useInvestigationStore.getState().pendingStart;
    let finish!: (value: unknown) => void;
    localHealthResponse = () => new Promise((resolve) => { finish = resolve; });
    await userEvent.click(canvas.getByRole('button', { name: 'Check local Ollama' }));
    await expect(reply.getByLabelText('Question')).toHaveValue('Retain this parent and draft.');
    expect(canvasElement.querySelector('[data-reply-task="inv-inline-parent"]')).toBe(host);
    expect(useInvestigationStore.getState().pendingStart).toBe(pending);
    await userEvent.click(canvas.getByRole('button', { name: 'Workspace' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Agent Chats' }));
    finish('metadata_received');
    await expect(canvas.getByText('Local API not checked.')).toBeVisible();
    expect(canvas.queryByText('Last local check: successful metadata response.')).toBeNull();
    expect(investigationRequests.filter((item) => item.command === 'start_investigation')).toHaveLength(0);
    expect(investigationRequests.filter((item) => item.command === 'check_local_ollama')).toHaveLength(1);
  },
};
