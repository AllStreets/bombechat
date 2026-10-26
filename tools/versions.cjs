// The build's version, where it is written down: the Cargo workspace (and every crate in it, which inherits it),
// the app's tauri.conf.json, package.json and the simulator. versionProblems(root) lists every place that disagrees
// with the workspace, so a build can never name itself one version on its plate and another in its bundle.
// Run by tools/check.cjs; on its own: node tools/versions.cjs
const fs = require('fs'), path = require('path');

function versionProblems(root = path.resolve(__dirname, '..')) {
  const read = f => fs.readFileSync(path.join(root, f), 'utf8');
  const problems = [];
  const ws = /\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/m.exec(read('Cargo.toml'));
  if (!ws) return ['Cargo.toml has no [workspace.package] version'];
  const version = ws[1];
  // every crate the workspace builds takes the workspace's version
  const members = /members\s*=\s*\[([^\]]*)\]/.exec(read('Cargo.toml'))[1].match(/"([^"]+)"/g).map(m => m.slice(1, -1));
  for (const m of members) {
    const pkg = /\[package\]([^[]*)/.exec(read(path.join(m, 'Cargo.toml')))[1];
    const own = /^version\s*=\s*"([^"]+)"/m.exec(pkg);
    if (own ? own[1] !== version : !/^version\.workspace\s*=\s*true/m.test(pkg)) problems.push(`${m}/Cargo.toml: version ${own ? own[1] : 'missing'}, the workspace is ${version}`);
  }
  const say = (where, v) => { if (v !== version) problems.push(`${where}: version ${v}, the workspace is ${version}`); };
  say('app/src-tauri/tauri.conf.json', JSON.parse(read('app/src-tauri/tauri.conf.json')).version);
  say('package.json', JSON.parse(read('package.json')).version);
  const sim = /machine: \(\) => Promise\.resolve\(\{[^}]*version: '([^']+)'/.exec(read('app/ui/simulator.js'));
  say('app/ui/simulator.js', sim ? sim[1] : 'missing');
  return problems;
}

const workspaceVersion = (root = path.resolve(__dirname, '..')) => /\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/m.exec(fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8'))[1];

module.exports = { versionProblems, workspaceVersion };
if (require.main === module) {
  const p = versionProblems(process.argv[2] ? path.resolve(process.argv[2]) : undefined);
  for (const x of p) console.error('versions: ' + x);
  if (p.length) process.exit(1);
  console.log('versions: every source says ' + workspaceVersion(process.argv[2] ? path.resolve(process.argv[2]) : undefined));
}
