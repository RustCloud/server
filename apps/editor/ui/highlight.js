// Syntax highlighting for the editor: a few rules per language, each a
// regular expression and the class of what it matches. The text is scanned
// once with all rules of the language joined; the earliest match wins, and
// of matches at the same place the first rule. Good enough to read code by,
// not a parser. Rules use only non-capturing groups `(?:…)`.
(() => {
  const C_COMMENT = String.raw`\/\/.*|\/\*[\s\S]*?\*\/`;
  const STRINGS = String.raw`"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'`;
  const NUMBER = String.raw`\b(?:0x[\da-fA-F_]+|0b[01_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)[a-zA-Z_\d]*\b`;
  const words = (list) => String.raw`\b(?:${list.split(' ').join('|')})\b`;
  const CALL = String.raw`\b[A-Za-z_$][\w$]*(?=\s*\()`;
  const TYPE = String.raw`\b[A-Z][A-Za-z\d_]*\b`;

  /** A C-like language: its keywords and literals, maybe more strings. */
  const cLike = (keywords, literals = 'true false null', extra = []) => [
    ['comment', C_COMMENT],
    ...extra,
    ['string', STRINGS],
    ['keyword', words(keywords)],
    ['literal', words(literals)],
    ['number', NUMBER],
    ['function', CALL],
    ['type', TYPE],
  ];

  const JS_KEYWORDS =
    'async await break case catch class const continue debugger default delete do else export extends finally for ' +
    'from function get if import in instanceof let new of return set static super switch this throw try typeof var ' +
    'void while with yield as interface type enum implements private protected public readonly declare namespace keyof';
  const TEMPLATE = ['string', String.raw`\x60(?:\\.|[^\x60\\])*\x60`];

  const LANGUAGES = {
    js: cLike(JS_KEYWORDS, 'true false null undefined NaN Infinity', [TEMPLATE]),
    rust: cLike(
      'as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut ' +
        'pub ref return self Self static struct super trait type unsafe use where while',
      'true false None Some Ok Err',
      [
        ['meta', String.raw`#!?\[[^\]\n]*\]`],
        ['type', String.raw`'[a-z_]\w*\b(?!')`],
      ],
    ),
    go: cLike(
      'break case chan const continue default defer else fallthrough for func go goto if import interface map ' +
        'package range return select struct switch type var',
      'true false nil iota',
      [TEMPLATE],
    ),
    java: cLike(
      'abstract assert break case catch class const continue default do else enum extends final finally for goto ' +
        'if implements import instanceof interface native new package private protected public return static ' +
        'super switch synchronized this throw throws transient try var void volatile while record sealed fun val ' +
        'when object companion data override open internal lateinit by',
      'true false null',
      [['meta', String.raw`@\w+`]],
    ),
    c: cLike(
      'auto break case char const continue default do double else enum extern float for goto if inline int long ' +
        'register return short signed sizeof static struct switch typedef union unsigned void volatile while class ' +
        'namespace template typename public private protected virtual override new delete using operator friend ' +
        'bool constexpr nullptr this throw try catch noexcept',
      'true false NULL nullptr',
      [['meta', String.raw`^\s*#\s*\w+.*`]],
    ),
    csharp: cLike(
      'abstract as async await base break case catch class const continue default delegate do else enum event ' +
        'explicit extern finally fixed for foreach goto if implicit in interface internal is lock namespace new ' +
        'operator out override params private protected public readonly ref return sealed sizeof static struct ' +
        'switch this throw try typeof unchecked unsafe using var virtual void volatile while get set init record',
      'true false null',
      [['meta', String.raw`^\s*#\w+.*`]],
    ),
    swift: cLike(
      'class deinit enum extension func import init let protocol struct subscript typealias var break case continue ' +
        'default defer do else fallthrough for guard if in repeat return switch where while as catch is rethrows ' +
        'throw throws try async await private public internal fileprivate open static self Self',
      'true false nil',
    ),
    php: cLike(
      'abstract and as break case catch class clone const continue declare default do echo else elseif empty ' +
        'enddeclare endfor endforeach endif endswitch endwhile extends final finally fn for foreach function global ' +
        'if implements include instanceof interface isset list match namespace new or print private protected ' +
        'public require return static switch throw trait try unset use var while yield',
      'true false null TRUE FALSE NULL',
      [
        ['comment', String.raw`#.*`],
        ['variable', String.raw`\$\w+`],
      ],
    ),
    dart: cLike(
      'abstract as assert async await break case catch class const continue default do else enum export extends ' +
        'external factory final finally for get if implements import in is late library mixin new on part required ' +
        'return set static super switch sync this throw try typedef var void while with yield',
      'true false null',
    ),
    python: [
      ['comment', String.raw`#.*`],
      ['string', String.raw`(?:[rbuf]|rb|br|fr|rf)?(?:"""[\s\S]*?"""|'''[\s\S]*?''')`],
      ['string', String.raw`(?:[rbuf]|rb|br|fr|rf)?(?:${STRINGS})`],
      ['meta', String.raw`^\s*@[\w.]+`],
      [
        'keyword',
        words(
          'and as assert async await break class continue def del elif else except finally for from global if ' +
            'import in is lambda nonlocal not or pass raise return try while with yield match case self',
        ),
      ],
      ['literal', words('True False None')],
      ['number', NUMBER],
      ['function', CALL],
      ['type', TYPE],
    ],
    ruby: [
      ['comment', String.raw`#.*`],
      ['string', STRINGS],
      ['literal', String.raw`:\w+|\b(?:true|false|nil)\b`],
      ['variable', String.raw`@{1,2}\w+|\$\w+`],
      [
        'keyword',
        words(
          'alias and begin break case class def defined do else elsif end ensure for if in module next not or redo ' +
            'rescue retry return self super then undef unless until when while yield require attr_accessor',
        ),
      ],
      ['number', NUMBER],
      ['type', TYPE],
    ],
    lua: [
      ['comment', String.raw`--\[\[[\s\S]*?\]\]|--.*`],
      ['string', STRINGS],
      [
        'keyword',
        words('and break do else elseif end for function goto if in local not or repeat return then until while'),
      ],
      ['literal', words('true false nil')],
      ['number', NUMBER],
      ['function', CALL],
    ],
    shell: [
      ['comment', String.raw`(?:^|\s)#.*`],
      ['string', String.raw`"(?:\\.|[^"\\])*"|'[^']*'`],
      ['variable', String.raw`\$(?:\{[^}\n]*\}|\w+|[@#?$!*-])`],
      [
        'keyword',
        words(
          'if then else elif fi for in do done while until case esac function return local export readonly ' +
            'declare set unset shift exit break continue source alias echo cd',
        ),
      ],
      ['number', String.raw`\b\d+\b`],
    ],
    powershell: [
      ['comment', String.raw`<#[\s\S]*?#>|#.*`],
      ['string', STRINGS],
      ['variable', String.raw`\$[\w:]+`],
      ['keyword', String.raw`\b(?:[Ii]f|[Ee]lse|[Ee]lse[Ii]f|[Ff]or|[Ff]or[Ee]ach|[Ww]hile|[Ff]unction|[Rr]eturn|[Pp]aram|[Tt]ry|[Cc]atch)\b`],
      ['function', String.raw`\b[A-Z][a-z]+-[A-Z]\w+\b`],
      ['number', NUMBER],
    ],
    sql: {
      flags: 'i',
      rules: [
        ['comment', String.raw`--.*|\/\*[\s\S]*?\*\/`],
        ['string', String.raw`'(?:''|[^'])*'`],
        ['variable', String.raw`"(?:""|[^"])*"|\x60[^\x60]*\x60`],
        [
          'keyword',
          words(
            'select from where and or not insert into values update set delete create table index view drop alter ' +
              'add column primary key foreign references on join left right inner outer full cross group by order ' +
              'having limit offset as distinct union all exists in is between like case when then else end begin ' +
              'commit rollback transaction default unique check constraint returning with recursive if cascade',
          ),
        ],
        ['literal', words('null true false')],
        ['type', words('int integer bigint smallint text varchar char boolean bool date timestamp timestamptz uuid numeric real float double serial bytea json jsonb blob')],
        ['number', NUMBER],
        ['function', CALL],
      ],
    },
    json: [
      ['property', String.raw`"(?:\\.|[^"\\\n])*"(?=\s*:)`],
      ['string', String.raw`"(?:\\.|[^"\\\n])*"`],
      ['literal', words('true false null')],
      ['number', String.raw`-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b`],
      ['comment', C_COMMENT],
    ],
    yaml: [
      ['comment', String.raw`(?:^|\s)#.*`],
      ['meta', String.raw`^(?:---|\.\.\.)\s*$`],
      ['property', String.raw`^[ \t]*(?:- +)?[^\s#:'"][^:\n#]*?(?=:(?:\s|$))`],
      ['string', STRINGS],
      ['variable', String.raw`[&*][\w-]+|!!?\w+`],
      ['literal', String.raw`(?<=:\s*|-\s+)(?:true|false|null|yes|no|on|off|~)\b`],
      ['number', String.raw`(?<=:\s*|-\s+)-?\d+(?:\.\d+)?\b`],
    ],
    toml: [
      ['comment', String.raw`[#;].*`],
      ['meta', String.raw`^\s*\[\[?[^\]\n]*\]\]?`],
      ['property', String.raw`^\s*[\w."' -]+?(?=\s*=)`],
      ['string', String.raw`"""[\s\S]*?"""|'''[\s\S]*?'''|${STRINGS}`],
      ['literal', words('true false')],
      ['number', String.raw`[+-]?\b\d[\d_:.TZ-]*\b`],
    ],
    env: [
      ['comment', String.raw`^\s*#.*`],
      ['keyword', String.raw`^\s*export\b`],
      ['property', String.raw`^\s*[\w.]+(?==)`],
      ['string', STRINGS],
      ['variable', String.raw`\$\{?\w+\}?`],
    ],
    markup: [
      ['comment', String.raw`<!--[\s\S]*?-->`],
      ['meta', String.raw`<![A-Za-z][^>]*>|<\?[\s\S]*?\?>|<!\[CDATA\[[\s\S]*?\]\]>`],
      ['tag', String.raw`<\/?[A-Za-z][\w:.-]*|\/?>`],
      ['property', String.raw`\b[\w:.@-]+(?=\s*=\s*["'])`],
      ['string', String.raw`"[^"]*"|'[^']*'`],
      ['variable', String.raw`&#?\w+;`],
    ],
    css: [
      ['comment', String.raw`\/\*[\s\S]*?\*\/|\/\/.*`],
      ['string', STRINGS],
      ['keyword', String.raw`@[\w-]+|!important\b`],
      ['property', String.raw`(?:--)?[\w-]+(?=\s*:[^:{;]*[;}\n])`],
      ['variable', String.raw`\$[\w-]+|var\(--[\w-]+\)`],
      ['number', String.raw`#[\da-fA-F]{3,8}\b|-?\b\d*\.?\d+(?:px|em|rem|%|vh|vw|s|ms|deg|fr|ch|ex|pt)?\b`],
      ['tag', String.raw`[.#][\w-]+|::?[\w-]+`],
      ['function', CALL],
    ],
    markdown: [
      ['meta', String.raw`^(?:\x60{3,}|~{3,})[^\n]*\n[\s\S]*?^(?:\x60{3,}|~{3,})\s*$`],
      ['keyword', String.raw`^ {0,3}#{1,6}[ \t].*`],
      ['comment', String.raw`^ {0,3}>.*|<!--[\s\S]*?-->`],
      ['string', String.raw`\x60[^\x60\n]+\x60`],
      ['tag', String.raw`!?\[[^\]\n]*\]\([^)\n]*\)|<https?:\/\/[^>\s]+>`],
      ['bold', String.raw`\*\*[^*\n]+\*\*|__[^_\n]+__`],
      ['italic', String.raw`(?<![*\w])\*[^*\n]+\*(?!\*)|(?<![_\w])_[^_\n]+_(?![_\w])`],
      ['number', String.raw`^[ \t]*(?:[-*+]|\d+[.)])(?=[ \t])|^ {0,3}(?:[-*_][ \t]*){3,}$`],
    ],
    dockerfile: [
      ['comment', String.raw`^\s*#.*`],
      [
        'keyword',
        String.raw`^\s*(?:FROM|RUN|CMD|LABEL|EXPOSE|ENV|ADD|COPY|ENTRYPOINT|VOLUME|USER|WORKDIR|ARG|ONBUILD|STOPSIGNAL|HEALTHCHECK|SHELL|MAINTAINER)\b|\bAS\b`,
      ],
      ['string', STRINGS],
      ['variable', String.raw`\$\{?\w+\}?`],
      ['property', String.raw`--[\w-]+`],
    ],
    makefile: [
      ['comment', String.raw`^\s*#.*`],
      ['property', String.raw`^[\w.%/ -]+(?=\s*:(?!=))`],
      ['variable', String.raw`\$[({][^)}\n]*[)}]|\$[@<^?*%]`],
      ['keyword', String.raw`^\s*(?:include|ifeq|ifneq|ifdef|ifndef|else|endif|define|endef|export)\b|\.PHONY\b`],
      ['string', STRINGS],
    ],
    diff: [
      ['meta', String.raw`^(?:@@.*|diff .*|index .*|--- .*|\+\+\+ .*)`],
      ['inserted', String.raw`^\+.*`],
      ['deleted', String.raw`^-.*`],
    ],
  };

  /** File name → language, by extension or whole name. */
  const BY_EXTENSION = {
    js: 'js', mjs: 'js', cjs: 'js', jsx: 'js', ts: 'js', tsx: 'js', mts: 'js', cts: 'js',
    rs: 'rust', go: 'go',
    java: 'java', kt: 'java', kts: 'java', scala: 'java', groovy: 'java', gradle: 'java',
    c: 'c', h: 'c', cc: 'c', cpp: 'c', cxx: 'c', hpp: 'c', hh: 'c', ino: 'c', proto: 'c',
    cs: 'csharp', swift: 'swift', php: 'php', dart: 'dart',
    py: 'python', pyw: 'python', rb: 'ruby', lua: 'lua',
    sh: 'shell', bash: 'shell', zsh: 'shell', fish: 'shell', ksh: 'shell', bashrc: 'shell', zshrc: 'shell', profile: 'shell',
    ps1: 'powershell', psm1: 'powershell',
    sql: 'sql',
    json: 'json', jsonl: 'json', json5: 'json', geojson: 'json', webmanifest: 'json', ipynb: 'json',
    yml: 'yaml', yaml: 'yaml',
    toml: 'toml', ini: 'toml', cfg: 'toml', conf: 'toml', properties: 'toml', editorconfig: 'toml',
    gitconfig: 'toml', service: 'toml', desktop: 'toml', gitattributes: 'env', gitignore: 'env',
    env: 'env',
    html: 'markup', htm: 'markup', xhtml: 'markup', xml: 'markup', svg: 'markup', vue: 'markup', svelte: 'markup',
    xsd: 'markup', xsl: 'markup', plist: 'markup', csproj: 'markup',
    css: 'css', scss: 'css', sass: 'css', less: 'css',
    md: 'markdown', markdown: 'markdown', mdx: 'markdown',
    dockerfile: 'dockerfile', containerfile: 'dockerfile',
    makefile: 'makefile', mk: 'makefile',
    diff: 'diff', patch: 'diff',
  };

  /** The language of a file, or null to show plain text. */
  function languageOf(name, mime) {
    const lower = name.toLowerCase();
    const extension = lower.includes('.') ? lower.slice(lower.lastIndexOf('.') + 1) : lower;
    if (BY_EXTENSION[extension]) return BY_EXTENSION[extension];
    if (lower.startsWith('dockerfile') || lower.endsWith('.dockerfile')) return 'dockerfile';
    if (lower.startsWith('.env')) return 'env';
    const byMime = {
      'application/json': 'json',
      'text/markdown': 'markdown',
      'text/html': 'markup',
      'text/xml': 'markup',
      'application/xml': 'markup',
      'text/css': 'css',
      'text/javascript': 'js',
      'application/javascript': 'js',
      'application/x-sh': 'shell',
      'application/sql': 'sql',
      'text/x-python': 'python',
      'application/yaml': 'yaml',
      'text/x-yaml': 'yaml',
    };
    return byMime[(mime ?? '').split(';')[0]] ?? null;
  }

  const compiled = new Map();

  function compile(language) {
    if (compiled.has(language)) return compiled.get(language);
    const spec = LANGUAGES[language];
    const rules = Array.isArray(spec) ? spec : spec.rules;
    const flags = 'gm' + (Array.isArray(spec) ? '' : spec.flags);
    const regex = new RegExp(rules.map(([, source]) => `(${source})`).join('|'), flags);
    const result = { regex, classes: rules.map(([name]) => name) };
    compiled.set(language, result);
    return result;
  }

  function escape(text) {
    return text.replace(/[&<>]/g, (c) => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : '&gt;'));
  }

  /** `text` as HTML with `<span class="hl-…">` around what the language's rules match. */
  function highlight(text, language) {
    if (!language || !LANGUAGES[language]) return escape(text);
    const { regex, classes } = compile(language);
    regex.lastIndex = 0;
    let html = '';
    let last = 0;
    let match;
    while ((match = regex.exec(text)) !== null) {
      if (match[0] === '') {
        regex.lastIndex += 1;
        continue;
      }
      const rule = match.findIndex((group, i) => i > 0 && group !== undefined) - 1;
      html += escape(text.slice(last, match.index));
      html += `<span class="hl-${classes[rule]}">${escape(match[0])}</span>`;
      last = match.index + match[0].length;
    }
    return html + escape(text.slice(last));
  }

  window.highlighter = { highlight, languageOf, languages: Object.keys(LANGUAGES) };
})();
