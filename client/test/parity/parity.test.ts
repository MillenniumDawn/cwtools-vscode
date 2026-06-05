/**
 * Host-free engine parity harness.
 *
 * Drives both cwtools server binaries (the vanilla F# reference and the Rust
 * port) directly over stdio — no VS Code host — and asks them identical
 * questions about the sample mod. For each capability:
 *
 *   - the F# ("vanilla") test asserts the reference behavior actually works,
 *     which also catches a broken rules/setup,
 *   - the Rust test asserts the port reaches the same answer.
 *
 * The Rust tests are expected to FAIL until the Rust binary reaches parity.
 * That is the point: a red Rust test is the to-do list for the port. Do not
 * weaken these to make Rust pass; fix the Rust binary instead.
 *
 * Rules are not bundled. Provide them with CWTOOLS_PARITY_RULES (a folder of
 * .cwt files) or run `npm run test:parity`, which clones the Stellaris config
 * into .cwtools-parity/ first. Without rules the whole suite is skipped.
 */
import * as assert from 'assert';
import { existsSync } from 'fs';
import * as path from 'path';
import { EngineSession, Engine } from './engineSession';

const repoRoot = path.resolve(__dirname, '../../../../..');
const sampleRoot = path.join(repoRoot, 'client/test/sample');
const files = {
	event: path.join(sampleRoot, 'events/irm.txt'),
	effects: path.join(sampleRoot, 'common/scripted_effects/irm_scripted_effects.txt'),
	niche: path.join(sampleRoot, 'common/pop_faction_types/irm_regionalist.txt'),
};

function findRules(): string | null {
	const candidates = [
		process.env.CWTOOLS_PARITY_RULES,
		path.join(repoRoot, '.cwtools-parity/cwtools-stellaris-config/config'),
	].filter((p): p is string => !!p);
	return candidates.find(p => existsSync(p)) ?? null;
}

function serverExe(engine: Engine): string {
	const base = path.join(repoRoot, 'release/bin/server');
	return engine === 'fsharp'
		? path.join(base, 'linux-x64/CWTools Server')
		: path.join(base, 'cwtools-server/cwtools-server');
}

interface HoverProbe {
	kind: 'hover';
	name: string;
	file: string;
	line: number;
	character: number;
	expected: string[];
}
interface CompletionProbe {
	kind: 'completion';
	name: string;
	file: string;
	line: number;
	character: number;
	expected: string[];
}
type Probe = HoverProbe | CompletionProbe;

// Positions and expectations mirror the host-based suite. `expected` is the
// vanilla (F#) behavior; the F# test proves it, the Rust test chases it.
const probes: Probe[] = [
	{
		kind: 'hover', name: 'trigger hover: is_country_type documents the check + scope chain',
		file: files.event, line: 37, character: 45,
		expected: ['Checks if the country is a specific type', 'Country', 'ROOT', 'THIS'],
	},
	{
		kind: 'hover', name: 'trigger hover: is_homeworld documents the check + scope chain',
		file: files.event, line: 15, character: 20,
		expected: ["Checks if the planet is its owner's homeworld", 'Country', 'ROOT', 'THIS'],
	},
	{
		kind: 'hover', name: 'localization hover: resolves the localized string',
		file: files.effects, line: 36, character: 70,
		expected: ['Faction Governance'],
	},
	{
		kind: 'completion', name: 'completion: niche context offers the scripted effect labels',
		file: files.niche, line: 26, character: 41,
		expected: ['regionalist_dublicated', 'sector_policy_leadership'],
	},
];

const rulesLeaf = findRules();

(rulesLeaf ? describe : describe.skip)('Engine parity (host-free, F# reference vs Rust port)', function () {
	this.timeout(120000);

	const sessions: Partial<Record<Engine, EngineSession>> = {};

	before(async function () {
		if (!rulesLeaf) return;
		for (const engine of ['fsharp', 'rust'] as Engine[]) {
			const session = await EngineSession.start({
				serverExe: serverExe(engine),
				engine,
				language: 'stellaris',
				rulesLeaf,
				workspaceRoot: sampleRoot,
			});
			for (const f of Object.values(files)) session.openDocument(f);
			await session.waitUntilLoaded();
			sessions[engine] = session;
		}
	});

	after(function () {
		for (const s of Object.values(sessions)) s?.dispose();
	});

	async function runHover(engine: Engine, p: HoverProbe): Promise<{ actual: string; missing: string[] }> {
		const actual = await sessions[engine]!.hover(p.file, p.line, p.character);
		return { actual, missing: p.expected.filter(s => !actual.includes(s)) };
	}

	async function runCompletion(engine: Engine, p: CompletionProbe): Promise<{ count: number; missing: string[] }> {
		const labels = await sessions[engine]!.completionLabels(p.file, p.line, p.character);
		return { count: labels.length, missing: p.expected.filter(s => !labels.includes(s)) };
	}

	for (const p of probes) {
		// The F# reference must work — a failure here means the rules/setup is
		// broken, not that Rust is behind.
		it(`[fsharp] ${p.name}`, async function () {
			if (p.kind === 'hover') {
				const { actual, missing } = await runHover('fsharp', p);
				assert.ok(actual.length > 0, 'F# reference returned an empty hover — rules not loaded?');
				assert.deepStrictEqual(missing, [], `F# hover missing ${JSON.stringify(missing)}\nactual: ${actual}`);
			} else {
				const { count, missing } = await runCompletion('fsharp', p);
				assert.ok(count > 0, 'F# reference returned no completions');
				assert.deepStrictEqual(missing, [], `F# completion missing ${JSON.stringify(missing)} (got ${count} labels)`);
			}
		});

		// The Rust port chases the reference. Expected to fail until parity.
		it(`[rust] ${p.name}`, async function () {
			if (p.kind === 'hover') {
				const { actual, missing } = await runHover('rust', p);
				assert.deepStrictEqual(missing, [], `Rust hover missing ${JSON.stringify(missing)}\nactual: ${actual}`);
			} else {
				const { count, missing } = await runCompletion('rust', p);
				assert.deepStrictEqual(missing, [], `Rust completion missing ${JSON.stringify(missing)} (got ${count} labels)`);
			}
		});
	}
});
