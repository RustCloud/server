import { describe, expect, it } from 'vitest';
import { parseHash } from './hash';

describe('parseHash', () => {
  it('splits the path and the query', () => {
    const route = parseHash('#/files/abc?q=report%202024');
    expect(route.parts).toEqual(['files', 'abc']);
    expect(route.query.get('q')).toBe('report 2024');
  });

  it('treats an empty hash as no parts', () => {
    expect(parseHash('').parts).toEqual([]);
    expect(parseHash('#/').parts).toEqual([]);
  });

  it('decodes path segments', () => {
    expect(parseHash('#/s/a%2Fb').parts).toEqual(['s', 'a/b']);
  });
});
