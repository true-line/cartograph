import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, userEvent, within } from 'storybook/test';
import { CoverageMatrix } from './CoverageMatrix';

/** Mirrors `ingest::matrix::COVERAGE_MATRIX` rows for JavaScript. */
const meta = {
  title: 'Settings/CoverageMatrix',
  component: CoverageMatrix,
  args: {
    language: 'JavaScript',
    cells: [
      {
        language: 'JavaScript',
        framework: 'core',
        capability: 'imports',
        support: 'supported',
        note: '',
      },
      {
        language: 'JavaScript',
        framework: 'express',
        capability: 'http_endpoints',
        support: 'partial',
        note: 'ES module imports only; CommonJS require("express") is not recognized',
      },
      {
        language: 'JavaScript',
        framework: 'next',
        capability: 'http_endpoints',
        support: 'unsupported',
        note: 'pages/api and app-router route handlers are not recognized',
      },
    ],
  },
} satisfies Meta<typeof CoverageMatrix>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FoldsCellsAndNamesShortfalls: Story = {
  // AC-0253: the summary counts every support level; opening it lists each
  // cell, and partial/unsupported cells say what they miss.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByText('1 supported · 1 partial · 1 unsupported'));
    const cells = canvas.getByRole('list', { name: 'JavaScript coverage cells' });
    await expect(within(cells).getAllByRole('listitem')).toHaveLength(3);
    await expect(within(cells).getByText('language · imports')).toBeInTheDocument();
    await expect(within(cells).getByText('Partial')).toBeInTheDocument();
    await expect(within(cells).getByText(/CommonJS require/)).toBeInTheDocument();
    await expect(within(cells).getByText('next · HTTP endpoints')).toBeInTheDocument();
  },
};

export const RendersNothingWithoutCells: Story = {
  // A plugin or uncovered language has no compiled-in cells yet (AC-0263).
  args: { cells: [] },
  play: async ({ canvasElement }) => {
    await expect(canvasElement.querySelector('.coverage-matrix')).toBeNull();
  },
};
