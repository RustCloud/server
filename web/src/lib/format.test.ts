import { describe, expect, it } from 'vitest';
import { copyName, describeAgent, formatBytes } from './format';

describe('formatBytes', () => {
  it('uses the largest fitting unit', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(1023)).toBe('1023 B');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(10 * 1024 * 1024)).toBe('10 MB');
    expect(formatBytes(5 * 1024 ** 4)).toBe('5.0 TB');
  });
});

describe('copyName', () => {
  it('keeps the extension last', () => {
    expect(copyName('report.pdf', false)).toBe('report (copy).pdf');
    expect(copyName('archive.tar.gz', false)).toBe('archive.tar (copy).gz');
    expect(copyName('.bashrc', false)).toBe('.bashrc (copy)');
    expect(copyName('v1.0', true)).toBe('v1.0 (copy)');
  });
});

describe('describeAgent', () => {
  it('names the browser and the system', () => {
    const firefox = 'Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0';
    const chrome = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36';
    const safari = 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1';
    const edge = chrome + ' Edg/140.0';
    expect(describeAgent(firefox)).toBe('Firefox, Linux');
    expect(describeAgent(chrome)).toBe('Chrome, Windows');
    expect(describeAgent(safari)).toBe('Safari, iOS');
    expect(describeAgent(edge)).toBe('Edge, Windows');
    expect(describeAgent('curl/8.0')).toBe('curl/8.0');
  });
});
