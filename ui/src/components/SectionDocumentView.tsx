import { useState } from 'react';
import type { CuratedRead } from '../store';
import {
  KIND_MEANING, SYSTEM_SCOPE, curatedDraftStates, curatedReadStatesDrafts, scopeLabel,
} from '../sectionDocument';
import type {
  CuratedDraftRow, DocumentSection, DraftClaimKind, DraftDecision, SectionAddress,
  SectionDocument, StoredSectionDraft,
} from '../sectionDocument';
import { TierBadge } from './TierBadge';

export interface SectionDocumentViewProps {
  /** The host's document view; `null` when it has not been read. */
  document: SectionDocument | null;
  /** The curated read on screen: the only source of an accepted draft's
   *  state. */
  curated: CuratedRead | null;
  busy?: boolean;
  error?: string | null;
  /** Whether the core is reachable, so a review or a request can be sent. */
  canReview: boolean;
  onReload?: () => void;
  onDraft?: (address: SectionAddress) => void;
  onReview: (draft: StoredSectionDraft, decision: DraftDecision, note: string | null) => void;
  onOpenCitation?: (citationId: string) => void;
}

const CLAIM_KIND: Record<DraftClaimKind, string> = {
  implemented: 'Implemented',
  documented_intent: 'Documented intent',
  interpretation: 'Interpretation',
};

const REVIEW_LABEL: Record<DraftDecision, string> = {
  accepted: 'Accepted',
  rejected: 'Rejected',
  returned: 'Returned',
};

const ACTION_LABEL: Record<DraftDecision, string> = {
  accepted: 'Accept',
  rejected: 'Reject',
  returned: 'Return',
};

const SUBJECT_ID = /^F-[0-9a-f]{16}$/u;

function shortDraftId(id: string): string {
  return id.length > 12 ? id.slice(0, 12) : id;
}

/** What the next curated read does with an accepted draft, or why that is
 *  not known. Stated only for an accepted draft. */
function AcceptedState({ row, stated, onOpenCitation }: {
  row: CuratedDraftRow | undefined;
  stated: boolean;
  onOpenCitation?: (citationId: string) => void;
}) {
  if (row) {
    const changed = row.changed ?? [];
    const missing = row.missing ?? [];
    return (
      <>
        <p className="document-draft-state" data-testid="draft-state">
          State: <code>{row.state}</code>. {row.reason_sentence} (<code>{row.reason}</code>)
        </p>
        {/* SPEC-17 slice 3 (AC-0673): the host's two lists, never derived
            here. A changed fact still exists and opens; a missing one has
            no evidence to open. */}
        {changed.length > 0 && (
          <p className="document-draft-facts" data-testid="draft-changed">
            Changed since this draft was accepted:{' '}
            {changed.map((id) => (onOpenCitation ? (
              <button
                key={id}
                type="button"
                className="citation-link"
                aria-label={`Open changed fact ${id}`}
                onClick={() => onOpenCitation(id)}
              >
                {id}
              </button>
            ) : (
              <code key={id}>{id}</code>
            )))}
          </p>
        )}
        {missing.length > 0 && (
          <p className="document-draft-facts" data-testid="draft-missing">
            No longer in the graph:{' '}
            {missing.map((id) => <code key={id} className="document-missing-fact">{id}</code>)}
          </p>
        )}
      </>
    );
  }
  return (
    <p className="document-draft-state" data-testid="draft-state-unread">
      {stated
        ? 'State not shown: the curated read on screen was made before this review. Refresh it to see the state.'
        : 'State not shown: the curated projection was not read or not computed.'}
    </p>
  );
}

function DraftCard({ draft, row, stated, busy, canReview, onDraft, onReview, onOpenCitation }: {
  draft: StoredSectionDraft;
  row: CuratedDraftRow | undefined;
  stated: boolean;
  busy: boolean;
  canReview: boolean;
  onDraft?: (address: SectionAddress) => void;
  onReview: SectionDocumentViewProps['onReview'];
  onOpenCitation?: (citationId: string) => void;
}) {
  const [note, setNote] = useState('');
  const decision = draft.review_decision;
  const noteId = `draft-note-${draft.draft_id}`;
  // A successor cannot itself be returned (ADR-0064 decision 6).
  const actions = (['accepted', 'rejected', 'returned'] as const).filter(
    (item) => item !== decision && !(item === 'returned' && draft.replaces !== null),
  );
  const accepted = decision === 'accepted';
  const changed = new Set(accepted ? row?.changed ?? [] : []);
  // A re-draft is offered for the two states the host re-drafts (SPEC-17).
  const redraft = accepted && (row?.state === 'stale' || row?.state === 'unresolvable');
  return (
    <article className="document-draft" aria-label={`Draft ${shortDraftId(draft.draft_id)}`}>
      <header className="document-draft-header">
        <strong>
          Draft <code title={draft.draft_id}>{shortDraftId(draft.draft_id)}</code>
        </strong>
        <span>{scopeLabel(draft.draft.address.scope)}</span>
        <span className="document-draft-review" data-testid="draft-review">
          {decision ? REVIEW_LABEL[decision] : 'Awaiting review'}
        </span>
      </header>
      {/* R-INT-2: the tier and confidence in words and with the shared
          badge. Acceptance never raises them (R-INT-3). */}
      <p className="document-draft-annotation">
        <span className="curated-tier">T3</span> <TierBadge tier="InferredWeak" /> Written by{' '}
        <code>{draft.origin.role}</code>.{' '}
        {decision === 'accepted'
          ? 'Human-reviewed: accepted. Acceptance does not raise the tier.'
          : 'Not accepted: no document prints it.'}
      </p>
      {accepted && <AcceptedState row={row} stated={stated} onOpenCitation={onOpenCitation} />}
      {draft.replaces && (
        <p className="muted">
          Replaces returned draft <code title={draft.replaces}>{shortDraftId(draft.replaces)}</code>.
        </p>
      )}
      <ol className="document-claims">
        {draft.draft.claims.map((claim, index) => (
          <li key={index}>
            <span className="document-claim-statement">{claim.statement}</span>{' '}
            <span className="document-claim-kind">({CLAIM_KIND[claim.kind] ?? claim.kind})</span>
            <span className="document-claim-citations">
              {' '}Cites{' '}
              {claim.citations.map((id) => (onOpenCitation ? (
                <button
                  key={id}
                  type="button"
                  className="citation-link"
                  aria-label={`Open evidence for ${id}`}
                  onClick={() => onOpenCitation(id)}
                >
                  {id}
                </button>
              ) : (
                <code key={id}>{id}</code>
              )))}
            </span>
            {claim.citations.some((id) => changed.has(id)) && (
              <span className="document-claim-changed"> Cites a changed fact.</span>
            )}
          </li>
        ))}
      </ol>
      {draft.review_note && (
        <p className="document-draft-note">Review note: {draft.review_note}</p>
      )}
      <div className="document-draft-actions">
        <label htmlFor={noteId}>Review note (optional)</label>
        <textarea
          id={noteId}
          rows={2}
          maxLength={2000}
          value={note}
          disabled={busy || !canReview}
          onChange={(event) => setNote(event.target.value)}
        />
        <div className="document-draft-buttons">
          {actions.map((item) => (
            <button
              key={item}
              type="button"
              className="secondary-button"
              disabled={busy || !canReview}
              onClick={() => onReview(draft, item, note.trim() ? note.trim() : null)}
            >
              {ACTION_LABEL[item]}
            </button>
          ))}
          {redraft && onDraft && (
            <button
              type="button"
              disabled={busy || !canReview}
              onClick={() => onDraft(draft.draft.address)}
            >
              Re-draft this section
            </button>
          )}
        </div>
      </div>
    </article>
  );
}

function DraftRequest({ document, section, busy, canReview, onDraft }: {
  document: SectionDocument;
  section: DocumentSection;
  busy: boolean;
  canReview: boolean;
  onDraft: (address: SectionAddress) => void;
}) {
  const [subject, setSubject] = useState('');
  const scope = subject.trim();
  const invalid = scope !== '' && !SUBJECT_ID.test(scope);
  const inputId = `draft-subject-${section.number}`;
  return (
    <div className="document-draft-request">
      {section.subject && (
        <>
          <label htmlFor={inputId}>
            One {section.subject} by citation id (optional; empty drafts for the whole system)
          </label>
          <input
            id={inputId}
            type="text"
            value={subject}
            placeholder="F-0123456789abcdef"
            aria-invalid={invalid}
            disabled={busy || !canReview}
            onChange={(event) => setSubject(event.target.value)}
          />
          {invalid && (
            <span className="error" role="alert">
              A citation id is F- followed by 16 lowercase hex digits.
            </span>
          )}
        </>
      )}
      <button
        type="button"
        disabled={busy || !canReview || invalid}
        onClick={() => onDraft({
          catalogue_version: document.catalogue_version,
          document: document.document,
          section: section.number,
          scope: scope === '' ? SYSTEM_SCOPE : scope,
        })}
      >
        Draft this section
      </button>
    </div>
  );
}

function Section({ document, section, states, stated, busy, canReview, onDraft, onReview, onOpenCitation }: {
  document: SectionDocument;
  section: DocumentSection;
  states: Map<string, CuratedDraftRow>;
  stated: boolean;
  busy: boolean;
  canReview: boolean;
  onDraft?: (address: SectionAddress) => void;
  onReview: SectionDocumentViewProps['onReview'];
  onOpenCitation?: (citationId: string) => void;
}) {
  const headingId = `document-section-${section.number}`;
  return (
    <section className="document-section" aria-labelledby={headingId}>
      <h4 id={headingId}>
        {section.number} {section.title}{' '}
        <span className="document-kind" title={KIND_MEANING[section.kind]}>{section.kind}</span>
      </h4>
      <p className="muted">{KIND_MEANING[section.kind]}</p>
      {section.kind === 'P' && (
        <p>Compiled in <code>product-specification.md</code>; nothing is drafted here.</p>
      )}
      {section.kind === 'H' && (
        <p>Left blank for your team. Cartograph never fills it.</p>
      )}
      {section.drafted && section.drafts.length === 0 && (
        <p data-testid="section-not-drafted">Not drafted.</p>
      )}
      {section.drafts.map((draft) => (
        <DraftCard
          key={draft.draft_id}
          draft={draft}
          row={states.get(draft.draft_id)}
          stated={stated}
          busy={busy}
          canReview={canReview}
          onDraft={onDraft}
          onReview={onReview}
          onOpenCitation={onOpenCitation}
        />
      ))}
      {section.drafted && onDraft && (
        <DraftRequest
          document={document}
          section={section}
          busy={busy}
          canReview={canReview}
          onDraft={onDraft}
        />
      )}
    </section>
  );
}

/**
 * The document view of the Spec Workbench (SPEC-15 slice 7, ADR-0064,
 * AC-0669). Presentational: the sections and their kinds, the stored drafts
 * and their reviews are the host's document view, and the state of an
 * accepted draft is the curated read's. A drafted claim is always shown as
 * T3 / InferredWeak and never as a recovered fact (R-INT-2, R-INT-3).
 */
export function SectionDocumentView({
  document, curated, busy = false, error = null, canReview, onReload, onDraft, onReview,
  onOpenCitation,
}: SectionDocumentViewProps) {
  const states = curatedDraftStates(curated);
  const stated = curatedReadStatesDrafts(curated);
  const drafts = document?.sections.flatMap((section) => section.drafts) ?? [];
  const pending = drafts.filter((draft) => draft.review_decision === null).length;
  const accepted = drafts.filter((draft) => draft.review_decision === 'accepted').length;
  return (
    <section className="curated-section section-document" aria-labelledby="section-document-title">
      <header className="curated-section-header">
        <div>
          <h3 id="section-document-title">Product specification sections</h3>
          <p className="muted">
            Document D1 by section. A drafted claim is an agent's reading of the facts it cites:
            T3 / InferredWeak before and after review. A best-effort export prints an accepted
            draft, marked stale once a fact it cites has changed.
          </p>
        </div>
        {onReload && (
          <button type="button" className="secondary-button" disabled={busy} onClick={onReload}>
            Reload sections
          </button>
        )}
      </header>

      {error && <p className="error" role="alert">{error}</p>}

      {document === null ? (
        <p className="muted" data-testid="document-unread">
          The document view has not been read. Nothing is stated here about drafts.
        </p>
      ) : (
        <details className="document-sections">
          <summary>
            Sections and drafts: {document.sections.length} sections, {drafts.length}{' '}
            {drafts.length === 1 ? 'draft' : 'drafts'} ({pending} awaiting review, {accepted} accepted)
          </summary>
          {document.drafts_truncated && (
            <p className="curated-notice" role="note">
              More drafts are stored than are shown here; the newest are shown.
            </p>
          )}
          {document.sections.map((section) => (
            <Section
              key={section.number}
              document={document}
              section={section}
              states={states}
              stated={stated}
              busy={busy}
              canReview={canReview}
              onDraft={onDraft}
              onReview={onReview}
              onOpenCitation={onOpenCitation}
            />
          ))}
        </details>
      )}
    </section>
  );
}
