import { describe, expect, it } from 'vitest';
import type { AppManifest, FileNode } from './api';
import { isActive, mimeMatches, openerPath, openersFor, serviceOrigin, sidebarEntries } from './apps';

const files: AppManifest = {
  id: 'files',
  version: '0.1.0',
  api: '=0.1.0',
  name: { en: 'Files', ru: 'Файлы' },
  ui: { icon: 'folder', entry: '/files', routes: ['links', 'search'] },
};

describe('sidebarEntries', () => {
  it('uses the name in the current language, English otherwise', () => {
    expect(sidebarEntries([files], 'ru')[0].title).toBe('Файлы');
    expect(sidebarEntries([files], 'de')[0].title).toBe('Files');
  });

  it('highlights the entry route and the routes the app declares', () => {
    const [entry] = sidebarEntries([files], 'en');
    expect(entry.href).toBe('/files');
    expect(entry.routes).toEqual([['files'], ['links'], ['search']]);
    expect(isActive(entry, ['links'])).toBe(true);
    expect(isActive(entry, ['settings'])).toBe(false);
  });

  it('leaves out apps without pages and keeps the server order', () => {
    const hook: AppManifest = { ...files, id: 'telegram', ui: undefined };
    const notes: AppManifest = {
      ...files,
      id: 'notes',
      name: { en: 'Notes' },
      ui: { icon: 'note', entry: '/app/notes' },
    };
    const entries = sidebarEntries([files, hook, notes], 'en');
    expect(entries.map((e) => e.id)).toEqual(['files', 'notes']);
    expect(entries[1].routes).toEqual([['app', 'notes']]);
    expect(isActive(entries[1], ['app', 'notes', 'x'])).toBe(true);
    expect(isActive(entries[1], ['app', 'calendar']), 'another app').toBe(false);
  });
});

describe('openersFor', () => {
  const file = (name: string, mime_type: string | null, is_dir = false) =>
    ({ id: 'f1', name, mime_type, is_dir }) as FileNode;
  const office: AppManifest = {
    ...files,
    id: 'office',
    name: { en: 'Office', ru: 'Офис' },
    open_with: [
      { mime: ['application/vnd.oasis.opendocument.text'], page: '/edit', write: true },
      { mime: ['image/*'], page: '/view', layout: 'panel', label: { en: 'View', ru: 'Посмотреть' } },
    ],
  };

  it('matches exact types and wildcards, nothing for folders', () => {
    expect(mimeMatches(['image/*'], 'image/png')).toBe(true);
    expect(mimeMatches(['image/*'], 'imagex/png')).toBe(false);
    expect(mimeMatches(['text/plain'], 'text/plain; charset=utf-8')).toBe(true);
    expect(mimeMatches(['text/plain'], null)).toBe(false);
    expect(openersFor([office], file('a', 'image/png', true), 'en')).toEqual([]);
  });

  it('gives the page, rights, layout and label of each entry', () => {
    const [edit] = openersFor([office], file('a.odt', 'application/vnd.oasis.opendocument.text'), 'ru');
    expect(edit).toEqual({ appId: 'office', label: null, appName: 'Офис', page: '/edit', write: true, full: true });
    const [view] = openersFor([files, office], file('a.png', 'image/png'), 'ru');
    expect([view.label, view.write, view.full]).toEqual(['Посмотреть', false, false]);
    expect(openerPath(view, 'f 1')).toBe('/app/office/view?file=f%201');
  });
});

describe('serviceOrigin', () => {
  const own = 'https://cloud.example.com';

  it('allows the services the administrator approved', () => {
    expect(serviceOrigin('https://office.example.com/browser/x.html', ['https://office.example.com'], [], own)).toBe(
      'https://office.example.com',
    );
    expect(serviceOrigin('https://evil.example/x', ['https://office.example.com'], [], own)).toBeNull();
  });

  it('allows paths of the server that go to the app\'s service, and nothing else of the server', () => {
    const proxies = ['/browser', '/cool'];
    expect(serviceOrigin(`${own}/browser/abc/cool.html?WOPISrc=x`, [], proxies, own)).toBe(own);
    expect(serviceOrigin(`${own}/cool`, [], proxies, own)).toBe(own);
    expect(serviceOrigin(`${own}/coolness`, [], proxies, own)).toBeNull();
    expect(serviceOrigin(`${own}/api/v1/files`, [], proxies, own)).toBeNull();
    expect(serviceOrigin('https://other.example.com/browser/x', [], proxies, own)).toBeNull();
    expect(serviceOrigin('not a url', [], proxies, own)).toBeNull();
  });
});
