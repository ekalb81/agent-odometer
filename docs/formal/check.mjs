import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { controls } from './controls.mjs';

const [jarPath, ...requested] = process.argv.slice(2);
const source = fileURLToPath(new URL('.', import.meta.url));
const inputs = readdirSync(source).filter((name) => /\.(tla|cfg)$/.test(name)).sort();
const configs = inputs.filter((name) => name.endsWith('.cfg'));
const selected = requested.length
  ? requested.map((name) => name.endsWith('.cfg') ? name : `${name}.cfg`)
  : configs;

if (!jarPath || selected.length === 0 || selected.some((name) => !configs.includes(name))) {
  console.error('Usage: node docs/formal/check.mjs <tla2tools.jar> [config ...]');
  console.error('Available configurations:', configs.join(', '));
  process.exit(2);
}

const jar = resolve(jarPath);
if (!existsSync(jar)) {
  console.error('TLC jar not found:', jar);
  process.exit(2);
}
const javaArgs = ['-Xmx2g', '-XX:+UseParallelGC', '-jar', jar, '-workers', '2'];
const temp = realpathSync(tmpdir());
const scratch = mkdtempSync(join(temp, 'odometer-tlc-'));
let failed = false;

function checkControl(control, index) {
  const { module, label, predicate, check, from, to, file = module } = control;
  const target = join(scratch, file + '.tla');
  const original = readFileSync(target, 'utf8');
  const claim = predicate ? 'INVARIANT WitnessUnreachable' : check;
  let model;
  if (predicate) {
    model = original.replace(/^={4,}\s*$/m, () => 'WitnessUnreachable == ~(' + predicate + ')\n====\n');
  } else {
    if (original.split(from).length !== 2) throw new Error('Mutation anchor changed: ' + label);
    model = original.replace(from, () => to);
  }
  const cfg = 'control-' + index + '.cfg';
  writeFileSync(target, model);
  writeFileSync(join(scratch, cfg), 'SPECIFICATION Spec\n' + claim + '\n');
  try {
    const result = spawnSync('java', [...javaArgs, '-metadir', 'control-' + index,
      '-config', cfg, module], { cwd: scratch, encoding: 'utf8' });
    if (result.error) throw result.error;
    const output = result.stdout + result.stderr;
    const kind = claim.startsWith('PROPERTY') ? 'Action property' : 'Invariant';
    const expected = 'Error: ' + kind + ' ' + claim.split(' ')[1] + ' is violated';
    if (result.status === 0 || !output.split(/\r?\n/).some(line => line.startsWith(expected))) {
      throw new Error(label + ' did not produce its required counterexample.\n' + output);
    }
    console.log((predicate ? 'Witness' : 'Detected') + ' — ' + module + ': ' + label);
  } finally {
    writeFileSync(target, original);
  }
}

try {
  for (const name of inputs) {
    copyFileSync(join(source, name), join(scratch, name));
  }
  for (const cfg of selected) {
    const module = cfg.split('.')[0];
    if (!inputs.includes(`${module}.tla`)) {
      throw new Error(`${cfg} has no matching module ${module}.tla`);
    }
    console.log(`\nChecking ${cfg}`);
    const result = spawnSync('java', [
      ...javaArgs, '-metadir', `${cfg}-states`, '-config', cfg, module,
    ], { cwd: scratch, stdio: 'inherit' });
    if (result.error) throw result.error;
    failed ||= result.status !== 0;
  }
  if (!failed) {
    for (const [i, control] of controls.entries()) {
      if (selected.includes(control.module + '.cfg')) checkControl(control, i);
    }
  }
} catch (error) {
  failed = true;
  console.error(error.message);
} finally {
  if (failed) {
    console.error(`TLC scratch files retained at ${scratch}`);
    process.exitCode = 1;
  } else if (dirname(scratch) === temp && basename(scratch).startsWith('odometer-tlc-')) {
    rmSync(scratch, { recursive: true, force: true });
  }
}
