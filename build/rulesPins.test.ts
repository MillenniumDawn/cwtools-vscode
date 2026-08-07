import { afterEach, beforeEach, suite, test, vi } from 'vitest';
import * as assert from 'node:assert';

const state = vi.hoisted(() => ({
	games: '',
	manifest: '',
	heads: new Map<string, string>(),
	writes: [] as Array<[string, string]>,
	execCalls: [] as string[][],
}));

vi.mock('node:fs', () => ({
	readFileSync: (file: string): string => {
		if (file.endsWith('/client/extension/games.ts')) return state.games;
		if (file.endsWith('/rules-pins.json')) return state.manifest;
		throw new Error(`unexpected read: ${file}`);
	},
	writeFileSync: (file: string, value: string): void => {
		state.writes.push([file, value]);
	},
}));

vi.mock('node:child_process', () => ({
	execFileSync: (_command: string, args: string[]): string => {
		state.execCalls.push(args);
		const repo = args[1];
		const head = repo && state.heads.get(repo);
		if (!head) throw new Error(`unexpected repository: ${repo}`);
		return `${head}\tHEAD\n`;
	},
}));

import {
	GAMES,
	LANGUAGE_REPOS,
	RULES_MANIFEST_REVISION,
} from '../client/extension/games';

const pins = Object.fromEntries(GAMES.map((game) => [game.id, game.repoRef]));

function setInputs(revision = RULES_MANIFEST_REVISION): void {
	state.games = [
		`export const RULES_MANIFEST_REVISION = ${revision};`,
		...GAMES.map(
			(game) => `repoRef: '${pins[game.id]}', // 2026-01-01`,
		),
	].join('\n');
	state.manifest = JSON.stringify({ schema: 1, revision, pins });
	state.heads.clear();
	for (const game of GAMES) {
		state.heads.set(game.repo, pins[game.id]);
	}
}

async function runRulesPins(): Promise<void> {
	vi.resetModules();
	await import('./rulesPins');
}

function writeFor(suffix: string): string {
	const write = state.writes.find(([file]) => file.endsWith(suffix));
	assert.ok(write, `no write for ${suffix}`);
	return write[1];
}

const log = vi.hoisted(() => vi.fn());

suite('rulesPins', () => {
	beforeEach(() => {
		vi.restoreAllMocks();
		vi.clearAllMocks();
		vi.useFakeTimers();
		vi.setSystemTime(new Date('2026-08-07T00:00:00Z'));
		state.writes.length = 0;
		state.execCalls.length = 0;
		setInputs();
		vi.spyOn(console, 'log').mockImplementation(log);
	});

	afterEach(() => {
		vi.restoreAllMocks();
		vi.useRealTimers();
	});

	test('does not write when every upstream head matches the pins', async () => {
		await runRulesPins();

		assert.deepStrictEqual(state.writes, []);
		assert.deepStrictEqual(log.mock.calls, [['rules pins are already current']]);
	});

	test('updates both pin sets and increments the shared revision once', async () => {
		const oldRef = pins.hoi4;
		const newRef = 'f'.repeat(40);
		state.heads.set(
			GAMES.find((game) => game.id === 'hoi4')!.repo,
			newRef,
		);

		await runRulesPins();

		const games = writeFor('/client/extension/games.ts');
		assert.ok(
			games.includes(
				`RULES_MANIFEST_REVISION = ${RULES_MANIFEST_REVISION + 1}`,
			),
		);
		assert.ok(games.includes(`repoRef: '${newRef}', // 2026-08-07`));
		const manifest = JSON.parse(writeFor('/rules-pins.json')) as {
			revision: number;
			pins: Record<string, string>;
		};
		assert.strictEqual(manifest.revision, RULES_MANIFEST_REVISION + 1);
		assert.strictEqual(manifest.pins.hoi4, newRef);
		assert.strictEqual(manifest.pins.stellaris, pins.stellaris);
		assert.deepStrictEqual(log.mock.calls, [
			[
				`- \`hoi4\` ${LANGUAGE_REPOS.hoi4.repo}/compare/${oldRef}...${newRef}`,
			],
		]);
	});

	test('increments the revision once when multiple games move', async () => {
		const hoi4Ref = 'f'.repeat(40);
		const stellarisRef = 'e'.repeat(40);
		state.heads.set(
			GAMES.find((game) => game.id === 'hoi4')!.repo,
			hoi4Ref,
		);
		state.heads.set(
			GAMES.find((game) => game.id === 'stellaris')!.repo,
			stellarisRef,
		);

		await runRulesPins();

		const manifest = JSON.parse(writeFor('/rules-pins.json')) as {
			revision: number;
			pins: Record<string, string>;
		};
		assert.strictEqual(manifest.revision, RULES_MANIFEST_REVISION + 1);
		assert.strictEqual(manifest.pins.hoi4, hoi4Ref);
		assert.strictEqual(manifest.pins.stellaris, stellarisRef);
	});

	test('fails before fetching or writing when the manifest revision disagrees', async () => {
		setInputs(RULES_MANIFEST_REVISION + 1);

		await assert.rejects(() => runRulesPins(), /does not match games.ts revision/);
		assert.deepStrictEqual(state.execCalls, []);
		assert.deepStrictEqual(state.writes, []);
	});

	test('fails before fetching or writing when a manifest pin disagrees', async () => {
		const inconsistent = JSON.parse(state.manifest) as {
			pins: Record<string, string>;
		};
		inconsistent.pins.hoi4 = 'f'.repeat(40);
		state.manifest = JSON.stringify(inconsistent);

		await assert.rejects(() => runRulesPins(), /manifest pin does not match games.ts/);
		assert.deepStrictEqual(state.execCalls, []);
		assert.deepStrictEqual(state.writes, []);
	});
});
