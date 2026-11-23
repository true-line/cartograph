import type { SpecAssertion } from './store';

/** A configuration key the last recovery could not resolve, with the client
 *  calls that need it (#2474). Declaring it in the manifest's `[env]` (and the
 *  service that answers it in `[services]`) lets those calls link. */
export interface SuggestedKey {
  key: string;
  /** Client calls that need this key. */
  calls: number;
  /** Their source files, each once. */
  sites: string[];
}

export interface ManifestSuggestions {
  keys: SuggestedKey[];
  /** Calls whose whole URL is built at runtime: no manifest key can name
   *  them, so they stay gaps until the code passes a declared base. */
  computedCalls: number;
  /** Their source files, each once. */
  computedSites: string[];
}

const ENV_KEY = /^env key (\S+) /;
const RUNTIME_BASE = /^runtime base URL `(.+)` of .+ is not declared in the system manifest$/;
const COMPUTED = 'runtime-computed fetch URL';

/** `gap:fetch:{repo}@{path}@{byte}` names its call site as `path`. */
function site(subjectId: string): string {
  const rest = subjectId.startsWith('gap:fetch:') ? subjectId.slice('gap:fetch:'.length) : subjectId;
  const first = rest.indexOf('@');
  const last = rest.lastIndexOf('@');
  return first >= 0 && last > first ? rest.slice(first + 1, last) : rest;
}

/** Reads the gap register's client-call gaps into manifest suggestions. Only
 *  the recorded reason is used; nothing is guessed about values. */
export function manifestSuggestions(gaps: readonly SpecAssertion[]): ManifestSuggestions {
  const keys = new Map<string, { calls: number; sites: Set<string> }>();
  const computed = new Set<string>();
  let computedCalls = 0;
  for (const gap of gaps) {
    if (!gap.subject_id.startsWith('gap:fetch:')) continue;
    const prefix = `${gap.subject_kind}: `;
    if (!gap.summary.startsWith(prefix)) continue;
    const reason = gap.summary.slice(prefix.length);
    const key = ENV_KEY.exec(reason)?.[1] ?? RUNTIME_BASE.exec(reason)?.[1];
    if (key) {
      const entry = keys.get(key) ?? { calls: 0, sites: new Set<string>() };
      entry.calls += 1;
      entry.sites.add(site(gap.subject_id));
      keys.set(key, entry);
    } else if (reason === COMPUTED) {
      computedCalls += 1;
      computed.add(site(gap.subject_id));
    }
  }
  return {
    keys: [...keys]
      .map(([key, { calls, sites }]) => ({ key, calls, sites: [...sites].sort() }))
      .sort((a, b) => b.calls - a.calls || a.key.localeCompare(b.key)),
    computedCalls,
    computedSites: [...computed].sort(),
  };
}
