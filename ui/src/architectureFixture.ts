import type { ArchitectureFact, ArchitectureView, Tier } from './store';

/** A small SimpleLogin-shaped C4 model for tests and stories (#2049). */
function fact(id: string, kind: string, label: string, tier: Tier, path: string): ArchitectureFact {
  return {
    id,
    kind,
    label,
    provenance: {
      tier: tier === 'Confirmed' ? 'T1' : 'T2',
      confidence_tier: tier,
      evidence: [{ repo: 'app', path, byte_start: 120, byte_end: 480, commit_sha: 'abc1234' }],
      extractor_id: 'fixture',
      content_hash: `h-${id}`,
    },
  };
}

export const ARCHITECTURE_FIXTURE: ArchitectureView = {
  artifact_id: 'architecture',
  schema: 'cartograph.spec.architecture.v1',
  content_id: 'sha256:architecture-fixture',
  mode: 'best-effort',
  system: 'SimpleLogin',
  elements: [
    {
      id: 'e1', kind: 'person', name: 'Web user', technology: '', confidence: 'InferredStrong',
      description: 'Uses the web app in a browser.',
      facts: [fact('n-screen-1', 'Screen', 'Dashboard', 'InferredStrong', 'templates/dashboard/index.html')],
      facts_total: 1,
    },
    {
      id: 'e2', kind: 'container', name: 'Web app', technology: 'Python, Flask', confidence: 'Confirmed',
      description: 'Serves the dashboard and the REST API.',
      facts: [fact('n-service-app', 'Service', 'app', 'Confirmed', 'server.py')],
      facts_total: 40,
    },
    {
      id: 'e3', kind: 'container', name: 'Email handler', technology: 'Python', confidence: 'Confirmed',
      description: 'Receives mail for aliases and forwards it.',
      facts: [fact('n-service-email', 'Service', 'email_handler', 'Confirmed', 'email_handler.py')],
      facts_total: 3,
    },
    {
      id: 'e4', kind: 'store', name: 'PostgreSQL', technology: 'PostgreSQL', confidence: 'Confirmed',
      description: 'Users, aliases, mailboxes.',
      facts: [fact('n-db', 'DataStore', 'postgres', 'Confirmed', 'app/models.py')],
      facts_total: 1,
    },
    {
      id: 'e5', kind: 'external', name: 'Stripe', technology: 'HTTPS', confidence: 'InferredWeak',
      description: 'Takes payments.',
      facts: [fact('n-stripe', 'ExternalService', 'stripe', 'InferredWeak', 'app/payments.py')],
      facts_total: 1,
    },
    {
      id: 'e6', kind: 'external', name: 'Mail relay', technology: 'SMTP', confidence: 'Gap',
      description: 'Where outgoing mail goes; not recovered.', facts: [], facts_total: 0,
    },
  ],
  relations: [
    { from: 'e1', to: 'e2', label: 'Uses', technology: 'HTTPS', confidence: 'InferredStrong', facts: [fact('e-1', 'TRIGGERS', 'screen TRIGGERS route', 'InferredStrong', 'static/js/index.js')], facts_total: 1 },
    { from: 'e2', to: 'e4', label: 'Reads and writes', technology: 'SQL', confidence: 'Confirmed', facts: [fact('e-2', 'READS', 'route READS table', 'Confirmed', 'app/models.py')], facts_total: 25 },
    { from: 'e3', to: 'e4', label: 'Reads and writes', technology: 'SQL', confidence: 'Confirmed', facts: [fact('e-3', 'WRITES', 'handler WRITES table', 'Confirmed', 'email_handler.py')], facts_total: 4 },
    { from: 'e2', to: 'e5', label: 'Calls', technology: 'HTTPS', confidence: 'InferredWeak', facts: [fact('e-4', 'CALLS', 'route CALLS stripe', 'InferredWeak', 'app/payments.py')], facts_total: 1 },
    { from: 'e3', to: 'e6', label: 'Sends mail via', technology: 'SMTP', confidence: 'Gap', facts: [], facts_total: 0 },
  ],
  components: [
    {
      container: 'e2',
      elements: [
        { id: 'e2c1', kind: 'component', name: 'Alias', technology: 'area', confidence: 'InferredWeak', description: '12 files.', facts: [], facts_total: 12 },
        { id: 'e2c2', kind: 'component', name: 'Billing', technology: 'area', confidence: 'InferredWeak', description: '4 files.', facts: [], facts_total: 4 },
      ],
      relations: [
        { from: 'e1', to: 'e2c1', label: 'Uses', technology: 'HTTPS', confidence: 'InferredWeak', facts: [], facts_total: 2 },
        { from: 'e2c1', to: 'e2c2', label: 'Calls', technology: '', confidence: 'InferredWeak', facts: [], facts_total: 1 },
        { from: 'e2c2', to: 'e5', label: 'Calls', technology: 'HTTPS', confidence: 'InferredWeak', facts: [], facts_total: 1 },
        { from: 'e2c1', to: 'e4', label: 'Reads and writes', technology: 'SQL', confidence: 'InferredWeak', facts: [], facts_total: 9 },
      ],
      unassigned_files: 3,
    },
  ],
  areas_supplied: true,
};
