import { describe, expect, it } from 'vitest';

// #1769: AGENTS.md requires every component in this folder to ship with a
// `*.stories.tsx` next to it. The glob only lists file names; nothing is loaded.
const files = Object.keys(import.meta.glob('./*.tsx')).map((path) => path.slice(2));

describe('component stories', () => {
  it('every component file has a story next to it', () => {
    const components = files.filter((file) => !file.endsWith('.stories.tsx'));
    const stories = new Set(files.filter((file) => file.endsWith('.stories.tsx')));
    expect(components.length).toBeGreaterThan(0);
    const missing = components.filter(
      (file) => !stories.has(file.replace(/\.tsx$/, '.stories.tsx')),
    );
    expect(missing).toEqual([]);
  });
});
