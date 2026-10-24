// Finds web addresses (http, https, ws, wss and protocol-relative) in the app's interface files. The app runs with
// connect-src 'none' and ships every asset, so none should appear at all.
// Allowed: XML namespace names (identifiers that are never fetched), licence
// texts (which must stay as published), and comments inside vendored three.js.
const fs = require('fs');
const path = require('path');

const NAMESPACES = /^http:\/\/www\.w3\.org\/(2000\/svg|1999\/xhtml|1999\/xlink)$/;
const LICENCE = /^(LICENSE|OFL-[\w-]+\.txt)$/;
// http(s) and ws(s) addresses, and protocol-relative ones (//host.tld/...) that a page would fetch over its own scheme
const URL = /\b(?:https?|wss?):\/\/[^\s'"`)<>\\]+|(?<![\\:\w\/])\/\/[a-z0-9-]+(?:\.[a-z0-9-]+)+(?::\d+)?(?:\/[^\s'"`)<>\\]*)?/gi;

function files(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap(e => {
    const p = path.join(dir, e.name);
    return e.isDirectory() ? files(p) : [p];
  });
}

// Drops // and /* */ comments while leaving strings, template literals and
// regular expressions alone. Good enough for three.js's own sources.
function stripComments(src) {
  let out = '', i = 0, prev = '';
  while (i < src.length) {
    const c = src[i], n = src[i + 1];
    if (c === '/' && n === '/') { while (i < src.length && src[i] !== '\n') i++; continue; }
    if (c === '/' && n === '*') { const end = src.indexOf('*/', i + 2); i = end < 0 ? src.length : end + 2; continue; }
    if (c === '"' || c === "'" || c === '`') {
      let j = i + 1;
      while (j < src.length && src[j] !== c) j += src[j] === '\\' ? 2 : 1;
      out += src.slice(i, j + 1); i = j + 1; prev = c; continue;
    }
    // a regular expression literal starts where a value can't end
    if (c === '/' && /[(,=:[!&|?{};]/.test(prev)) {
      let j = i + 1, cls = false;
      while (j < src.length && (cls || src[j] !== '/') && src[j] !== '\n') { if (src[j] === '\\') j++; else if (src[j] === '[') cls = true; else if (src[j] === ']') cls = false; j++; }
      out += src.slice(i, j + 1); i = j + 1; prev = 'x'; continue;
    }
    out += c; if (!/\s/.test(c)) prev = c; i++;
  }
  return out;
}

function externalUrls(root) {
  const found = [];
  for (const f of files(root)) {
    const name = path.basename(f), rel = path.relative(root, f);
    if (LICENCE.test(name) || /\.(woff2|png|jpg|ico)$/.test(name)) continue;
    let text = fs.readFileSync(f, 'utf8');
    if (rel.split(path.sep)[0] === 'vendor') text = stripComments(text);
    for (const m of text.matchAll(URL)) if (!NAMESPACES.test(m[0])) found.push(`${rel}: ${m[0]}`);
  }
  return found;
}

module.exports = { externalUrls, stripComments };
if (require.main === module) {
  const found = externalUrls(path.resolve(process.argv[2] || path.join(__dirname, '..', 'app', 'ui')));
  console.log(found.length ? found.join('\n') : 'no external URLs');
  process.exit(found.length ? 1 : 0);
}
