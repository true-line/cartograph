import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { NavRail } from './NavRail';
import { SAVED_RESULT_NOTICE_LIMIT } from '../savedResultNoticeStore';

const meta = {
  title: 'Shell/NavRail',
  component: NavRail,
  args: {
    active: 'workspace',
    onNavigate: fn(),
    onOpenPalette: fn(),
  },
} satisfies Meta<typeof NavRail>;

export default meta;
type Story = StoryObj<typeof meta>;

export const AllSurfaces: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Every icon-only control is reachable by accessible name (handoff #4).
    for (const label of [
      'Workspace',
      'Atlas',
      'Flows',
      'Spec Workbench',
      'Gaps & Drift',
      'Provenance & Eval',
      'Jobs',
      'Settings',
      'Agent Chats',
      'Command palette',
    ]) {
      await expect(canvas.getByRole('button', { name: label })).toBeInTheDocument();
    }
    await expect(canvas.getByRole('button', { name: 'Agent Chats' })).not.toHaveAttribute('aria-describedby');
    await expect(canvasElement.querySelector('.nav-saved-result-badge')).toBeNull();
    // The active surface is exposed as the current page.
    await expect(canvas.getByRole('button', { name: 'Workspace' })).toHaveAttribute(
      'aria-current',
      'page',
    );

    await userEvent.click(canvas.getByRole('button', { name: 'Atlas' }));
    await expect(args.onNavigate).toHaveBeenCalledWith('atlas');
    await userEvent.click(canvas.getByRole('button', { name: 'Command palette' }));
    await expect(args.onOpenPalette).toHaveBeenCalled();
  },
};

export const AtlasActive: Story = {
  args: { active: 'atlas' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'Atlas' })).toHaveAttribute(
      'aria-current',
      'page',
    );
    await expect(canvas.getByRole('button', { name: 'Workspace' })).not.toHaveAttribute(
      'aria-current',
    );
  },
};

// AC-0412: known counts and uncertainty are descriptions, never a renamed destination.
export const KnownUnreadSavedResults: Story = {
  args: { active: 'agent-chats', knownUnreadSavedResults: SAVED_RESULT_NOTICE_LIMIT, unreadChecksIncomplete: false },
  play: async ({ canvasElement, args }) => {
    const button = within(canvasElement).getByRole('button', { name: 'Agent Chats' });
    const badge = button.querySelector<HTMLElement>('.nav-saved-result-badge')!;
    const count = args.knownUnreadSavedResults ?? 0;
    await expect(badge.textContent).toBe(`${count || ''}${args.unreadChecksIncomplete ? '?' : ''}`);
    await expect(button).toHaveAccessibleDescription(`Known unread saved results: ${count}.${args.unreadChecksIncomplete ? ' Some saved-result checks are incomplete.' : ''}`);
    await expect(button).toHaveAttribute('aria-current', 'page');
    await expect(badge).toBeVisible();
    const bounds = badge.getBoundingClientRect(); const parent = button.getBoundingClientRect();
    expect(bounds.width).toBeGreaterThan(0); expect(bounds.height).toBeGreaterThan(0);
    expect(bounds.left).toBeGreaterThanOrEqual(parent.left); expect(bounds.right).toBeLessThanOrEqual(parent.right);
    expect(bounds.top).toBeGreaterThanOrEqual(parent.top); expect(bounds.bottom).toBeLessThanOrEqual(parent.bottom);
    await userEvent.click(button); await expect(args.onNavigate).toHaveBeenCalledWith('agent-chats');
  },
};
export const IncompleteKnownUnreadResults: Story = {
  ...KnownUnreadSavedResults, args: { active: 'agent-chats', knownUnreadSavedResults: SAVED_RESULT_NOTICE_LIMIT, unreadChecksIncomplete: true },
};
export const IncompleteWithoutKnownUnreadResults: Story = {
  ...KnownUnreadSavedResults, args: { active: 'agent-chats', knownUnreadSavedResults: 0, unreadChecksIncomplete: true },
};
