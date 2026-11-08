import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { renderArtifactMarkdown } from '../markdownView';

const hash = 'a'.repeat(64);
const content = `| Endpoint | Method | Path | Tier | Confidence | Source | Content hash |
|---|---|---|---|---|---|---|
| Alias deletion | DELETE | /api/aliases/<int:alias_id> | Deterministic | Confirmed | ${'src/api/alias_handler.py:153; '.repeat(6)} | ${hash} |`;

function ArtifactTable({ width }: { width: number }) {
  return (
    <div style={{ width, maxWidth: '100%' }} className="spec-artifact-rendered md-view">
      {renderArtifactMarkdown(content)}
    </div>
  );
}

const meta = {
  title: 'Spec/ArtifactTable',
  component: ArtifactTable,
  args: { width: 420 },
} satisfies Meta<typeof ArtifactTable>;
export default meta;
type Story = StoryObj<typeof meta>;

// AC-0032: short labels remain readable; long evidence stays inside a scrollable table.
const checkLayout: NonNullable<Story['play']> = async ({ canvasElement }) => {
  await document.fonts.ready;
  const canvas = within(canvasElement);
  const method = canvas.getByRole('cell', { name: 'DELETE' });
  const label = canvas.getByRole('cell', { name: 'Alias deletion' });
  const evidence = canvas.getByRole('cell', { name: hash });
  const range = document.createRange();
  range.selectNodeContents(method);
  // The recorded HTTP method fits on one line, rather than one character per line.
  await expect(range.getBoundingClientRect().height).toBeLessThanOrEqual(
    Number.parseFloat(getComputedStyle(method).lineHeight) + 1,
  );
  range.selectNodeContents(label);
  await expect(range.getBoundingClientRect().height).toBeLessThanOrEqual(
    Number.parseFloat(getComputedStyle(label).lineHeight) * 2 + 1,
  );
  const wrapper = method.closest('.md-table-wrap') as HTMLElement;
  const preview = wrapper.parentElement!;
  await expect(wrapper.scrollWidth).toBeGreaterThan(wrapper.clientWidth);
  await expect(preview.scrollWidth).toBeLessThanOrEqual(preview.clientWidth + 1);
  wrapper.scrollLeft = wrapper.scrollWidth;
  await expect(wrapper.scrollLeft).toBeGreaterThan(0);
  await expect(evidence.getBoundingClientRect().right).toBeLessThanOrEqual(
    wrapper.getBoundingClientRect().right + 1,
  );
  await expect(evidence).toHaveTextContent(hash);
};

export const Narrow: Story = { play: checkLayout };
export const Wide: Story = { args: { width: 700 }, play: checkLayout };
