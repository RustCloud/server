import { describe, expect, it } from 'vitest';
import type { AppManifest } from './api';
import { appKinds, coreKinds, freeName } from './newfiles';

const office: AppManifest = {
  id: 'rc-office',
  version: '0.3.0',
  api: '^0.2.16',
  name: { en: 'Office' },
  new_files: [
    { extension: 'docx', label: { en: 'Document', ru: 'Документ' }, template: '/templates/empty.docx' },
    { extension: 'csv', label: { en: 'Table' } },
  ],
};

describe('new file kinds', () => {
  it('has text and Markdown of its own', () => {
    const kinds = coreKinds({ textFile: 'Text', textFileName: 'New', markdownFile: 'MD', markdownFileName: 'Note' });
    expect(kinds.map((k) => `${k.baseName}.${k.extension}`)).toEqual(['New.txt', 'Note.md']);
    expect(kinds.every((k) => !('appId' in k))).toBe(true);
  });

  it('takes the kinds apps add, labelled in the language or English', () => {
    const kinds = appKinds([{ ...office, new_files: undefined, id: 'notes' }, office], 'ru');
    expect(kinds.map((k) => [k.key, k.label, k.template])).toEqual([
      ['rc-office:docx', 'Документ', '/templates/empty.docx'],
      ['rc-office:csv', 'Table', undefined],
    ]);
    expect(kinds[0].appId).toBe('rc-office');
  });
});

describe('freeName', () => {
  it('numbers the name until it is free, ignoring case', () => {
    expect(freeName('Document', 'docx', [])).toBe('Document.docx');
    expect(freeName('Document', 'docx', ['document.DOCX', 'Document (2).docx'])).toBe('Document (3).docx');
    expect(freeName('Document', 'docx', ['Document.odt'])).toBe('Document.docx');
  });
});
