import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { AreaExpertPanel } from './AreaExpertPanel';
import type { AreaChoice, AreaExchange } from '../areaExpert';

// #2057, AC-0932: choose an area, ask its expert, and read an answer that is
// labelled agent-authored with the citations the briefing prints.
const AREAS: AreaChoice[] = [
  { area: 'subsystem:cluster:alias', name: 'Alias', files: 12, sections: ['1.3', '1.4', '1.5'], briefing_bytes: 98_304, briefing_digest: '5f1c0a9e2b7d4c38' },
  { area: 'subsystem:cluster:admin-ui', name: 'Admin UI', files: 1, sections: [], briefing_bytes: 420, briefing_digest: '0b3e6a1d9c2f7e45' },
];
const CITED = 'F-00000000000000a1';
const ANSWERED: AreaExchange = {
  question: 'What happens when an alias is deleted?',
  outcome: {
    outcome: 'answered', area: AREAS[0].area, area_name: 'Alias', role: 'topic-expert@1', job_id: '7',
    usage: { model: 'claude-opus', input_tokens: 24120, output_tokens: 512, request_bytes: 99000 },
    statements: [
      { statement: 'A deleted alias is moved to the trash first.', basis: 'stated', citations: [CITED] },
      { statement: 'Trashed aliases are purged by a scheduled job.', basis: 'inferred', citations: [CITED] },
    ],
    not_known: ['How long a trashed alias is kept.'],
    ask_instead: ['Mailbox'],
    left_out: 1,
  },
};
const REFUSED: AreaExchange = {
  question: 'And who may restore it?',
  outcome: {
    outcome: 'no_answer', reason: 'reply_not_the_schema',
    usage: { model: 'claude-opus', input_tokens: null, output_tokens: null, request_bytes: 99000 },
  },
};

const meta = {
  title: 'Spec/AreaExpertPanel',
  component: AreaExpertPanel,
  args: {
    areas: AREAS,
    selected: null,
    exchanges: [],
    busy: false,
    error: null,
    canAsk: true,
    onReload: fn(),
    onSelect: fn(),
    onAsk: fn(),
    onOpenCitation: fn(),
  },
} satisfies Meta<typeof AreaExpertPanel>;
export default meta;
type Story = StoryObj<typeof meta>;

export const ChooseAnArea: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { level: 3, name: "Ask an area's expert" })).toBeVisible();
    // Nothing can be asked before an area is chosen.
    await expect(canvas.queryByRole('button', { name: 'Review and ask' })).not.toBeInTheDocument();
    await userEvent.selectOptions(canvas.getByLabelText('Area'), 'Alias (12 files)');
    await expect(args.onSelect).toHaveBeenCalledWith('subsystem:cluster:alias');
  },
};

export const AskAndRead: Story = {
  args: { selected: AREAS[0].area, exchanges: [ANSWERED, REFUSED] },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('area-expert-briefing')).toHaveTextContent('Briefing: sections 1.3, 1.4, 1.5, 96 KB.');
    const answer = within(canvas.getByRole('article', { name: `Question: ${ANSWERED.question}` }));
    // Agent-authored, by role, never a recovered fact.
    await expect(answer.getByText('topic-expert@1')).toBeVisible();
    await expect(answer.getByText(/An agent's reading \(T3 \/ InferredWeak\), not a recovered fact\./)).toBeVisible();
    await expect(answer.getByText('A deleted alias is moved to the trash first.')).toBeVisible();
    await expect(answer.getByText('Stated')).toHaveAttribute('title', 'The briefing states this.');
    await expect(answer.getByText('Inferred')).toBeVisible();
    await expect(answer.getByTestId('area-not-known')).toHaveTextContent('How long a trashed alias is kept.');
    await expect(answer.getByTestId('area-ask-instead')).toHaveTextContent('Ask the expert of: Mailbox.');
    await expect(answer.getByText(/1 statement was left out/)).toBeVisible();
    // What the call used, as the provider reported it.
    await expect(answer.getByTestId('area-usage')).toHaveTextContent(
      'claude-opus: 24,120 input and 512 output tokens, as the provider reported.',
    );
    await userEvent.click(answer.getAllByRole('button', { name: `Open evidence for ${CITED}` })[0]);
    await expect(args.onOpenCitation).toHaveBeenCalledWith(CITED);
    // A refusal is put into words.
    const refused = within(canvas.getByRole('article', { name: `Question: ${REFUSED.question}` }));
    await expect(refused.getByRole('status')).toHaveTextContent('The model did not reply with the answer schema.');
    await expect(refused.getByTestId('area-usage')).toHaveTextContent('97 KB sent; the provider reported no token count.');

    // The question goes out trimmed; a blank one does not.
    const ask = canvas.getByRole('button', { name: 'Review and ask' });
    await expect(ask).toBeDisabled();
    await userEvent.type(canvas.getByLabelText('Question for the Alias expert'), '  Who may delete an alias? ');
    await userEvent.click(ask);
    await expect(args.onAsk).toHaveBeenCalledWith('Who may delete an alias?');
  },
};

export const AreaWithNothingListed: Story = {
  args: { selected: AREAS[1].area },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('area-expert-briefing')).toHaveTextContent(
      'The product specification lists nothing under this area',
    );
  },
};

export const NoAreasOrCore: Story = {
  args: { areas: [], canAsk: false, error: 'The core is not running.' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('area-expert-empty')).toBeVisible();
    await expect(canvas.getByRole('alert')).toHaveTextContent('The core is not running.');
  },
};
