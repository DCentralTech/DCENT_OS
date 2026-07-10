// Dump the body objects defined in cypress/support/e2e.ts to a JSON file
// so the preview-with-mocks.py server can use the same data the e2e
// tests use. Strips numeric underscores + the `Cypress.on` runtime
// hooks, then evaluates only the `const X = {...}` declarations.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import vm from 'node:vm';

const __dirname = dirname(fileURLToPath(import.meta.url));
const src = readFileSync(resolve(__dirname, '../cypress/support/e2e.ts'), 'utf-8');

// Extract every top-level `const NAME = { ... };` block by brace-matching.
const fixtures = {};
const re = /^const\s+(\w+)\s*=\s*\{/gm;
let m;
while ((m = re.exec(src))) {
  const name = m[1];
  let i = m.index + m[0].length - 1; // position of opening `{`
  let depth = 0;
  let end = -1;
  for (let j = i; j < src.length; j++) {
    const c = src[j];
    if (c === '{') depth++;
    else if (c === '}') {
      depth--;
      if (depth === 0) { end = j + 1; break; }
    }
  }
  if (end < 0) continue;
  const body = src.slice(i, end);
  try {
    // Safe eval: strict numeric-underscore strip + vm sandbox with `Date` available.
    const cleaned = body.replace(/(\d)_(?=\d)/g, '$1');
    const value = vm.runInNewContext('(' + cleaned + ')', { Date });
    fixtures[name] = value;
  } catch (e) {
    console.error(`skip ${name}: ${e.message}`);
  }
}

writeFileSync(resolve(__dirname, 'preview-fixtures.json'),
  JSON.stringify(fixtures, null, 2));
console.error(`[fixtures] dumped ${Object.keys(fixtures).length} fixtures: ${Object.keys(fixtures).join(', ')}`);
