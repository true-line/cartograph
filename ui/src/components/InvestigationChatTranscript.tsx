import type { InvestigationChat } from '../investigationTypes';
import type { InvestigationChatStatus } from '../investigationStore';

export interface InvestigationChatTranscriptProps {
  chat: InvestigationChat | null;
  status: InvestigationChatStatus;
  error: string | null;
  onRefresh: () => void;
}

/** Renders only the persisted user question and typed result reference. */
export function InvestigationChatTranscript({ chat, status, error, onRefresh }: InvestigationChatTranscriptProps) {
  return <section className="investigation-section" aria-label="Saved investigation chat">
    <h3>Conversation</h3>
    {status === 'loading' && <p role="status">Loading saved conversation…</p>}
    {status === 'preview' && <p className="muted">Saved conversations are available when Cartograph is running in the desktop app.</p>}
    {status === 'legacy' && <p className="muted">No saved conversation is available for this older investigation. Its findings remain available below.</p>}
    {status === 'error' && <div role="alert" className="error-text">
      <p>{error ?? 'The saved conversation could not be loaded.'}</p>
      <button type="button" className="secondary-button" onClick={onRefresh}>Refresh conversation</button>
    </div>}
    {status === 'saved' && chat && <>
      <p className="muted">Saved producer: <code>{chat.producer.specialist_id}</code> · version {chat.producer.specialist_version} ·
        {' '}{chat.producer.provider_mode} · {chat.producer.provider_id}/{chat.producer.model} ·
        {' '}tier ceiling {chat.producer.tier}/{chat.producer.confidence_tier}</p>
      <ol className="investigation-chat-messages">
        {chat.messages.map((entry) => entry.message.kind === 'user_question'
          ? <li key={entry.id} aria-label="User question">
            <strong>You</strong><p>{entry.message.question}</p>
          </li>
          : <li key={entry.id} aria-label="Specialist result reference">
            <strong>Specialist result</strong>
            <p>Saved result <code>{entry.message.result_id}</code></p>
            <a href="#investigation-findings">View admitted findings and citations</a>
          </li>)}
      </ol>
    </>}
  </section>;
}
