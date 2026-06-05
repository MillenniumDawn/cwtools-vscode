// Fetches the Stellaris config rules the parity harness compares against.
// Idempotent: clones once into .cwtools-parity/ and is a no-op afterwards.
// Honours CWTOOLS_PARITY_RULES (skip the clone, use an existing checkout).
import { existsSync, mkdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import * as path from 'node:path';

const repoRoot = path.resolve(__dirname, '../../..');
const REPO = 'https://github.com/cwtools/cwtools-stellaris-config';

if (process.env.CWTOOLS_PARITY_RULES) {
	console.log(`[parity] using CWTOOLS_PARITY_RULES=${process.env.CWTOOLS_PARITY_RULES}`);
	process.exit(0);
}

const dest = path.join(repoRoot, '.cwtools-parity/cwtools-stellaris-config');
const config = path.join(dest, 'config');

if (existsSync(config)) {
	console.log(`[parity] rules present at ${config}`);
	process.exit(0);
}

mkdirSync(path.dirname(dest), { recursive: true });
console.log(`[parity] cloning ${REPO} -> ${dest}`);
const r = spawnSync('git', ['clone', '--depth', '1', REPO, dest], { stdio: 'inherit' });
if (r.status !== 0) {
	console.error('[parity] clone failed; the parity suite will skip.');
	process.exit(0); // non-fatal: the suite self-skips without rules
}
console.log(`[parity] rules ready at ${config}`);
