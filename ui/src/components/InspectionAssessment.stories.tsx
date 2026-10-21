import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { InspectionAssessment } from './InspectionAssessment';
import { inspectionAssessment, inspectionTarget } from '../investigationFixtures';

const meta = { title: 'Investigations/InspectionAssessment', component: InspectionAssessment,
  args: { requirements: [inspectionTarget], assessment: inspectionAssessment(), location: null,
    locationTargetId: null, locationLoading: false, locationError: null, onLocation: fn() },
} satisfies Meta<typeof InspectionAssessment>;
export default meta;
type Story = StoryObj<typeof meta>;

export const AcquiredButOmittedIsUnmet: Story = {
  // AC-0261: source acquisition is separate from actual finishing invocation supply.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/Requirement unmet/)).toBeVisible();
    await expect(canvas.getByText('Read by specialist: yes · Available when specialist finished: no · Cited: no')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Show captured line/column' }));
    await expect(args.onLocation).toHaveBeenCalledWith(inspectionTarget);
  },
};

export const SuppliedButUncitedIsSeparate: Story = {
  args: { assessment: inspectionAssessment(true) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/Requirement met: exact source was available/)).toBeVisible();
    await expect(canvas.getByText('Read by specialist: yes · Available when specialist finished: yes · Cited: no')).toBeVisible();
    await expect(canvas.getByText(/Uncited source is not shown as support/)).toBeVisible();
  },
};

export const LegacyNotAssessed: Story = {
  args: { requirements: [], assessment: undefined },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Source inspections: Not assessed')).toBeVisible();
    await expect(canvas.queryByText(/Requirement met:/)).not.toBeInTheDocument();
  },
};

export const RunningTaskAwaitsAssessment: Story = {
  args: { assessment: undefined },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Source inspections: Awaiting assessment')).toBeVisible();
    await expect(canvas.queryByText('Source inspections: Not assessed')).not.toBeInTheDocument();
  },
};

export const ExactCapturedCoordinates: Story = {
  // AC-0262: display host-provided retained units; never infer lines from opaque IDs.
  args: { locationTargetId: inspectionTarget.target_id, location: {
    target_id: inspectionTarget.target_id, captured: inspectionTarget.captured, byte_start: 12, byte_end: 54,
    start_line: 1, start_column: 13, end_line: 2, end_column: 23, column_unit: 'utf8_bytes', end_exclusive: true,
  } },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText(/Captured location: line 1, column 13 to line 2, column 23/)).toBeVisible();
    await expect(within(canvasElement).getByText(/1-based; UTF-8 byte columns; end exclusive/)).toBeVisible();
  },
};
