/** One repo's coverage inventory (AC-0238, `ingest::coverage`). */
export type ProducerState =
  "ran" | "not_selected" | "not_applicable" | "missing_input";

/** Why the plugin host refused one file's extraction (AC-0658). */
export type RefusalReason = "malformed_fact" | "guest_error" | "bound_exceeded";

/** The files a gated plugin was refused on. No fact from them by that
 *  plugin joined the graph, and they are not in the producer's `files`. */
export interface PluginRefusals {
  malformed_fact: number;
  guest_error: number;
  bound_exceeded: number;
  /** Refused files in path order, bounded by the inventory. */
  files: { path: string; reason: RefusalReason }[];
  /** Refused files counted above but not listed. */
  omitted: number;
}

/** The not-covered capabilities the inventory records for refused files,
 *  one per reason class. They are shown with the plugin that was refused. */
export const REFUSAL_CAPABILITIES: readonly string[] = [
  "plugin-malformed-fact",
  "plugin-guest-error",
  "plugin-bound-exceeded",
];

export const REFUSAL_REASON_TEXT: Record<RefusalReason, string> = {
  malformed_fact: "the plugin emitted a fact the host does not accept",
  guest_error: "the plugin returned an error or trapped",
  bound_exceeded: "the plugin exceeded its fuel, memory or table bound",
};

export interface ProducerCoverage {
  id: string;
  kind: "deterministic" | "observational";
  covers: string;
  state: ProducerState;
  files: number;
  nodes: number;
  edges: number;
  inputs: string[];
  stopping_reason: string;
  /** Present only for a gated plugin that was refused on a file. */
  refusals?: PluginRefusals | null;
}

export interface MissingInput {
  input: string;
  producer: string;
  reason: string;
}

export interface UnsupportedCapability {
  capability: string;
  occurrences: number;
  first_site: string;
  reason: string;
}

export interface RepoCoverage {
  inventory: string;
  repo: string;
  commit: string;
  commit_note?: string;
  layer_hints: string[];
  producers: ProducerCoverage[];
  missing_inputs: MissingInput[];
  unsupported: UnsupportedCapability[];
  limited: boolean;
}

/** One repo's recorded inventory read back from the store (AC-0700). A
 *  stale one carries no body: nothing is read from it. */
export interface RestoredCoverage {
  repo: string;
  status: "recorded" | "stale";
  inventory_hash: string;
  /** What was compared with the current state at read time. */
  compared: string[];
  inventory: RepoCoverage | null;
}

/** The one-line verdict for a repo. A limited inventory names what limits
 *  it, and none ever reads as exhaustive: "complete" means only the
 *  declared repo, revision, layer hints and inputs. */
export function coverageVerdict(coverage: RepoCoverage): string {
  const ran = coverage.producers.filter((p) => p.state === "ran").length;
  const excluded = coverage.producers.filter(
    (p) => p.state === "not_selected",
  ).length;
  const parts = [`${ran} producer${ran === 1 ? "" : "s"} ran`];
  if (excluded > 0) parts.push(`${excluded} excluded by layer hints`);
  if (coverage.missing_inputs.length > 0) {
    parts.push(
      `${coverage.missing_inputs.length} missing input${coverage.missing_inputs.length === 1 ? "" : "s"}`,
    );
  }
  const unsupported = unsupportedRows(coverage).length;
  if (unsupported > 0) {
    parts.push(
      `${unsupported} unsupported capabilit${unsupported === 1 ? "y" : "ies"}`,
    );
  }
  const refused = refusedFileCount(coverage);
  if (refused > 0) {
    parts.push(
      `${refused} file${refused === 1 ? "" : "s"} refused for a plugin`,
    );
  }
  const scope = coverage.limited
    ? "Limited coverage"
    : "Complete within the declared scope only";
  return `${scope} · ${parts.join(" · ")}`;
}

/** Producers worth a row: those that ran or were held back. Producers with
 *  nothing to read collapse into one line, so the list stays short. */
export function producerRows(coverage: RepoCoverage): {
  shown: ProducerCoverage[];
  notApplicable: string[];
} {
  return {
    shown: coverage.producers.filter((p) => p.state !== "not_applicable"),
    notApplicable: coverage.producers
      .filter((p) => p.state === "not_applicable")
      .map((p) => p.covers),
  };
}

/** Files refused for any gated plugin in this repo, listed or not. */
export function refusedFileCount(coverage: RepoCoverage): number {
  return coverage.producers.reduce(
    (sum, producer) =>
      sum +
      (producer.refusals
        ? producer.refusals.malformed_fact +
          producer.refusals.guest_error +
          producer.refusals.bound_exceeded
        : 0),
    0,
  );
}

/** Unsupported capabilities other than refused plugin files, which are
 *  listed with the plugin that was refused. */
export function unsupportedRows(
  coverage: RepoCoverage,
): UnsupportedCapability[] {
  return coverage.unsupported.filter(
    (entry) => !REFUSAL_CAPABILITIES.includes(entry.capability),
  );
}
