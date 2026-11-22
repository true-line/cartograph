import { SPECIALIST_TABLE, type SpecialistRole } from './generated/specialistTable';
import { isExpandedSpecialist } from './investigationFamilies';
import { investigationPolicy } from './investigationObligations';
import type { InvestigationCatalog, InvestigationSpecialist, SpecialistId } from './investigationTypes';

/** ADR-0071 decision 1: the user picks a role; its version is the role's current one. */
export const SPECIALIST_ROLES: readonly SpecialistRole[] = ['domain-analyst', 'evidence-auditor', 'spec-writer', 'gap-triager', 'section-writer'];
export const ROLE_LABELS: Readonly<Record<SpecialistRole, string>> = {
  'domain-analyst': 'Domain analyst',
  'evidence-auditor': 'Evidence auditor',
  'spec-writer': 'Spec writer',
  'gap-triager': 'Gap triager',
  'section-writer': 'Section writer',
};

export const roleOf = (id: string): SpecialistRole | undefined =>
  Object.hasOwn(SPECIALIST_TABLE, id) ? SPECIALIST_TABLE[id as SpecialistId].role : undefined;

/** ADR-0071 decision 2: a retired version only reopens; it starts no new work. */
export const isRetired = (id: string): boolean =>
  Object.hasOwn(SPECIALIST_TABLE, id) && SPECIALIST_TABLE[id as SpecialistId].availability === 'retired';

/** The investigation profile a version runs under; the user never picks it separately. */
export function profileFor(catalog: InvestigationCatalog | null | undefined, id: SpecialistId): string {
  if (!isExpandedSpecialist(id)) return 'investigation-v1';
  return catalog?.policies?.find((policy) => policy.limits.profile !== 'investigation-v1' && policy.specialist_ids.includes(id))?.limits.profile ??
    'investigation-v2';
}

/** Versions of a role the catalog offers for a new start, newest first. */
export function offeredVersions(catalog: InvestigationCatalog | null | undefined, role: SpecialistRole): InvestigationSpecialist[] {
  return (catalog?.specialists ?? [])
    .filter((item) => roleOf(item.id) === role && Boolean(investigationPolicy(catalog ?? null, profileFor(catalog, item.id), item.id)))
    .sort((left, right) => right.version - left.version);
}

/** The role's current version when the catalog offers it, else its newest offered version. */
export function currentVersion(catalog: InvestigationCatalog | null | undefined, role: SpecialistRole): InvestigationSpecialist | undefined {
  const offered = offeredVersions(catalog, role);
  return offered.find((item) => SPECIALIST_TABLE[item.id].availability === 'current') ?? offered[0];
}

/** Roles the catalog offers at least one version of, in display order. */
export const offeredRoles = (catalog: InvestigationCatalog | null | undefined) =>
  SPECIALIST_ROLES.filter((role) => offeredVersions(catalog, role).length > 0);
