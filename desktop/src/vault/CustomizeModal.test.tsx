import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CustomizeModal, { type CustomizeInit } from './CustomizeModal';
import { HUES } from './vaultMeta';

afterEach(cleanup);

const init = (over: Partial<CustomizeInit> = {}): CustomizeInit => ({ id: 'vid', name: 'Notes', hue: 222, emoji: null, ...over });

describe('CustomizeModal', () => {
  // grid emoji cells carry the asp-hover-list class (category tabs do not).
  const gridEmoji = (container: HTMLElement, ch: string) =>
    (Array.from(container.querySelectorAll('.asp-hover-list')) as HTMLElement[]).find((e) => e.textContent === ch)!;

  it('edits name, color, emoji and saves', () => {
    const onSave = vi.fn();
    const onCancel = vi.fn();
    const { container } = render(<CustomizeModal initial={init()} onSave={onSave} onCancel={onCancel} />);

    fireEvent.change(screen.getByDisplayValue('Notes'), { target: { value: 'Work log' } });
    fireEvent.click(screen.getByTestId(`swatch-${HUES[2]}`));
    fireEvent.click(gridEmoji(container, '😀'));
    fireEvent.click(screen.getByText('Save'));

    expect(onSave).toHaveBeenCalledWith({ id: 'vid', name: 'Work log', hue: HUES[2], emoji: '😀' });
  });

  it('searches emojis, shows no-results, and clears search via category', () => {
    const { container } = render(<CustomizeModal initial={init()} onSave={vi.fn()} onCancel={vi.fn()} />);
    const search = screen.getByPlaceholderText('Search emojis');
    fireEvent.change(search, { target: { value: 'rocket' } });
    expect(gridEmoji(container, '🚀')).toBeTruthy();
    fireEvent.change(search, { target: { value: 'zzzznope' } });
    expect(screen.getByText('No emoji found')).toBeTruthy();
    // clearing the query restores category tabs; click one
    fireEvent.change(search, { target: { value: '' } });
    fireEvent.click(screen.getByTitle('Animals'));
    expect(gridEmoji(container, '🐶')).toBeTruthy();
  });

  it('removes an emoji icon (back to monogram) and defaults a blank name', () => {
    const onSave = vi.fn();
    render(<CustomizeModal initial={init({ emoji: '📓', name: '' })} onSave={onSave} onCancel={vi.fn()} />);
    // "Remove icon" appears because an emoji is set.
    fireEvent.click(screen.getByText('Remove icon'));
    fireEvent.click(screen.getByText('Save'));
    expect(onSave).toHaveBeenCalledWith({ id: 'vid', name: 'Untitled vault', hue: 222, emoji: null });
  });

  it('cancels via button and overlay', () => {
    const onCancel = vi.fn();
    const { container } = render(<CustomizeModal initial={init()} onSave={vi.fn()} onCancel={onCancel} />);
    fireEvent.click(screen.getByText('Cancel'));
    fireEvent.click(container.firstChild as HTMLElement); // overlay
    expect(onCancel).toHaveBeenCalledTimes(2);
  });
});
