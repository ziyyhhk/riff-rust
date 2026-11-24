// Cross-platform equivalent of scripts/assemble.ps1, used by CI on Linux.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const web = path.join(root, 'web');

function copyDir(src, dest) {
  if (!fs.existsSync(src)) return;
  fs.mkdirSync(dest, { recursive: true });
  for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
    const from = path.join(src, entry.name);
    const to = path.join(dest, entry.name);
    if (entry.isDirectory()) copyDir(from, to);
    else fs.copyFileSync(from, to);
  }
}

function walk(dir, out = []) {
  if (!fs.existsSync(dir)) return out;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

fs.rmSync(web, { recursive: true, force: true });
fs.mkdirSync(web, { recursive: true });
copyDir(path.join(root, 'src', 'renderer'), web);
copyDir(path.join(root, 'assets'), path.join(web, 'assets'));
copyDir(path.join(root, 'vendor'), path.join(web, 'vendor'));

const files = walk(web).filter((f) => ['.html', '.js', '.css', '.json'].includes(path.extname(f)));
for (const f of files) {
  const original = fs.readFileSync(f, 'utf8');
  let text = original
    .replaceAll('../../assets/', 'assets/')
    .replaceAll('../../vendor/', 'vendor/')
    .replaceAll('../../src/renderer/defaults.json', 'defaults.json');
  if (f.endsWith(path.join('web', 'index.html'))) {
    text = text.replace(/<title>[^<]*<\/title>/, '<title>Riff Rust</title>');
  }
  if (text !== original) fs.writeFileSync(f, text);
}

console.log(`Assembled web/ (${files.length} files rewritten)`);
