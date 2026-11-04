import { describe, expect, it } from 'vitest';
import { statusItems } from './api';

describe('statusItems', () => {
  it('takes a list of lines with a level and text or text by language', () => {
    const items = statusItems([
      { level: 'ok', text: { en: 'Connected', ru: 'Подключено' } },
      { level: 'error', text: 'No model' },
    ]);
    expect(items.map((item) => item.level)).toEqual(['ok', 'error']);
  });

  it('refuses anything else', () => {
    for (const answer of [{}, [{ level: 'fine', text: 'x' }], [{ level: 'ok' }], [{ level: 'ok', text: { en: 1 } }]]) {
      expect(() => statusItems(answer)).toThrow();
    }
  });
});
