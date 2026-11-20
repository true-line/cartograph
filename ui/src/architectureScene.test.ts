import { describe, expect, it } from 'vitest';
import {
  SYSTEM_ID,
  componentsScene,
  containersScene,
  containersWithComponents,
  contextScene,
  lowestTier,
} from './architectureScene';
import { ARCHITECTURE_FIXTURE as VIEW } from './architectureFixture';

// #2049 slice 2: the scenes of the Architecture view come from the model alone.
describe('architecture scenes', () => {
  it('takes the lowest confidence', () => {
    expect(lowestTier([])).toBe('Confirmed');
    expect(lowestTier(['Confirmed', 'InferredWeak', 'InferredStrong'])).toBe('InferredWeak');
    expect(lowestTier(['Confirmed', 'Gap'])).toBe('Gap');
  });

  it('level 1 folds containers and stores into one system box', () => {
    const scene = contextScene(VIEW);
    expect(scene.nodes.map((n) => n.id)).toEqual(['e1', SYSTEM_ID, 'e5', 'e6']);
    const system = scene.nodes.find((n) => n.id === SYSTEM_ID)!;
    expect(system.description).toBe('2 containers and 1 data store');
    expect(system.drillable).toBe(true);
    // Store relations stay inside the box; the rest cross it, one arrow per pair.
    expect(scene.edges.map((e) => `${e.from}->${e.to}`)).toEqual([
      `e1->${SYSTEM_ID}`,
      `${SYSTEM_ID}->e5`,
      `${SYSTEM_ID}->e6`,
    ]);
    const gap = scene.edges.find((e) => e.to === 'e6')!;
    expect(gap.confidence).toBe('Gap');
    expect(gap.relations).toHaveLength(1);
  });

  it('level 2 places containers and stores inside the system boundary', () => {
    const scene = containersScene(VIEW);
    const inside = scene.nodes.filter((n) => n.parent === SYSTEM_ID).map((n) => n.id);
    expect(inside).toEqual(['e2', 'e3', 'e4']);
    expect(scene.nodes.find((n) => n.id === 'e2')!.drillable).toBe(true);
    expect(scene.nodes.find((n) => n.id === 'e3')!.drillable).toBe(false);
    expect(scene.edges).toHaveLength(VIEW.relations.length);
    // People above, stores below containers, outside systems lowest.
    const y = (id: string) => scene.nodes.find((n) => n.id === id)!.y;
    expect(y('e1')).toBeLessThan(y('e2'));
    expect(y('e2')).toBeLessThan(y('e4'));
    expect(y('e4')).toBeLessThan(y('e5'));
  });

  it('level 3 draws one container with its callers above and callees below', () => {
    expect(containersWithComponents(VIEW).map((e) => e.id)).toEqual(['e2']);
    const scene = componentsScene(VIEW, 'e2')!;
    const boundary = 'e2:boundary';
    expect(scene.nodes.filter((n) => n.parent === boundary).map((n) => n.id)).toEqual([
      'e2c1',
      'e2c2',
    ]);
    const y = (id: string) => scene.nodes.find((n) => n.id === id)!.y;
    expect(y('e1')).toBeLessThan(y('e2c1'));
    expect(y('e5')).toBeGreaterThan(y('e2c2'));
    expect(y('e4')).toBeGreaterThan(y('e2c2'));
    expect(componentsScene(VIEW, 'e3')).toBeNull();
  });

  it('is deterministic', () => {
    expect(containersScene(VIEW)).toEqual(containersScene(structuredClone(VIEW)));
  });
});
