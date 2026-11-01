import { describe, expect, it } from 'vitest';
import { whyTier } from './components/EvidencePanel';

// AC-0558 (T-0558, #1601): the producer comes from the recorded producing tier,
// never from confidence; confidence is described separately.
describe('evidence drawer tier explanation', () => {
  it('names the semantic producer of a weak fact instead of the agentic tier', () => {
    const text = whyTier('InferredWeak', 'Semantic');
    expect(text).toMatch(/semantic tier \(T2\)/);
    expect(text).not.toMatch(/agentic/);
    expect(text).toMatch(/Inferred \(weak\)/);
  });

  it('calls a dynamic inferred fact observed execution, not static analysis', () => {
    const text = whyTier('InferredStrong', 'Dynamic');
    expect(text).toMatch(/observed execution \(T1\)/);
    expect(text).not.toMatch(/static analysis|semantic|agentic/);
    expect(text).toMatch(/Inferred \(strong\)/);
  });

  it('keeps deterministic and agentic producers distinct from confidence', () => {
    expect(whyTier('InferredStrong', 'Deterministic')).toMatch(/static analysis \(T0\).*Inferred \(strong\)/);
    expect(whyTier('InferredWeak', 'Agentic')).toMatch(/agentic tier \(T3\).*Inferred \(weak\)/);
    expect(whyTier('Confirmed', 'Deterministic')).toMatch(/Confirmed: proven outright/);
  });

  it('says so when the producing tier is absent or unknown', () => {
    expect(whyTier('InferredWeak', undefined)).toMatch(/^The producing tier is not recorded/);
    expect(whyTier('InferredStrong', 'Mystery')).toMatch(/^The producing tier is not recorded/);
  });
});
