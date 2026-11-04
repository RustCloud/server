import { describe, expect, it } from 'vitest';
import { copyName, formatBytes } from './format';

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
