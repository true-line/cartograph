/** One repo's coverage inventory (AC-0238, `ingest::coverage`). */
export type ProducerState =
  "ran" | "not_selected" | "not_applicable" | "missing_input";

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
  if (coverage.unsupported.length > 0) {
    parts.push(
      `${coverage.unsupported.length} unsupported capabilit${coverage.unsupported.length === 1 ? "y" : "ies"}`,
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
