import type { SystemRepo } from './store';

/** AC-0144: operational names never hide distinct current source identities.
 * Labels are display-only; matching always uses the full exact repository key. */
export function createRepositoryLabeler(
  sources: readonly Pick<SystemRepo, 'repo' | 'display_name'>[] = [],
): (repo: string) => string {
  const names = new Map(
    sources.map(({ repo, display_name }) => [repo, display_name?.trim() || repo]),
  );
  const counts = new Map<string, number>();
  for (const name of names.values()) {
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return (repo) => {
    const name = names.get(repo);
    if (!name || name === repo) return repo;
    return (counts.get(name) ?? 0) > 1 ? `${name} (${repo})` : name;
  };
}

/** The recovered system's name (#2469): one source is named by its label,
 *  several by their count. Null when no evidence-backed source is recorded. */
export function systemDisplayName(
  sources: readonly Pick<SystemRepo, 'repo' | 'display_name'>[],
): string | null {
  if (sources.length === 0) return null;
  if (sources.length === 1) return createRepositoryLabeler(sources)(sources[0].repo);
  return `${sources.length} repos as one system`;
}

/** Host text names a source by its exact key; show its label there instead
 *  (#2471). Display only: the key is never rewritten anywhere it is matched. */
export function relabelRepoText(text: string, repo: string, label: string): string {
  return label === repo ? text : text.split(repo).join(label);
}
