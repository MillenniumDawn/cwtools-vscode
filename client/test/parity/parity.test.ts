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
	triggers: path.join(sampleRoot, 'common/scripted_triggers/irm_scripted_triggers.txt'),
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
interface DefinitionProbe {
	kind: 'definition';
	name: string;
	file: string;
	line: number;
	character: number;
	/** Expected target file (basename) the definition should resolve to. */
	expectedTargetFile: string;
}
interface ReferencesProbe {
	kind: 'references';
	name: string;
	file: string;
	line: number;
	character: number;
	/** Minimum number of references expected (including the declaration). */
	expectedMinCount: number;
}
interface DiagnosticsProbe {
	kind: 'diagnostics';
	name: string;
	file: string;
	/** True = expect at least one diagnostic; false = expect zero. */
	expectAny: boolean;
}
interface FormattingProbe {
	kind: 'formatting';
	name: string;
	file: string;
	/** True = expect at least one edit; false = expect zero. */
	expectAny: boolean;
}
type Probe =
	| HoverProbe
	| CompletionProbe
	| DefinitionProbe
	| ReferencesProbe
	| DiagnosticsProbe
	| FormattingProbe;

// Positions and expectations mirror the host-based suite. `expected` is the
// vanilla (F#) behavior; the F# test proves it, the Rust test chases it.
const probes: Probe[] = [
	// --- hover ---
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
	// --- completion ---
	{
		kind: 'completion', name: 'completion: niche context offers the scripted effect labels',
		file: files.niche, line: 26, character: 41,
		expected: ['regionalist_dublicated', 'sector_policy_leadership'],
	},
	// --- definition ---
	{
		kind: 'definition', name: 'goto-def: pop_can_politics resolves to the scripted trigger',
		file: files.niche, line: 35, character: 3,
		expectedTargetFile: 'irm_scripted_triggers.txt',
	},
	// --- references ---
	{
		kind: 'references', name: 'find-refs: pop_can_politics has multiple usage sites',
		file: files.triggers, line: 86, character: 0,
		expectedMinCount: 4,
	},
	// --- diagnostics ---
	{
		kind: 'diagnostics', name: 'diagnostics: event file is parsed without errors',
		file: files.event,
		expectAny: false,
	},
	// --- formatting ---
	{
		kind: 'formatting', name: 'formatting: returns edits for the event file',
		file: files.event,
		expectAny: true,
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

	async function runDefinition(engine: Engine, p: DefinitionProbe): Promise<{ targets: string[] }> {
		const defs = await sessions[engine]!.definition(p.file, p.line, p.character);
		const targets = defs.map(d => path.basename(new URL(d.uri).pathname));
		return { targets };
	}

	async function runReferences(engine: Engine, p: ReferencesProbe): Promise<{ count: number }> {
		const refs = await sessions[engine]!.references(p.file, p.line, p.character);
		return { count: refs.length };
	}

	function runDiagnostics(engine: Engine, p: DiagnosticsProbe): { count: number } {
		const diags = sessions[engine]!.getDiagnostics(p.file);
		return { count: diags.length };
	}

	async function runFormatting(engine: Engine, p: FormattingProbe): Promise<{ count: number }> {
		// Give the server a moment to finish any lingering parse before formatting.
		await new Promise<void>(r => setTimeout(r, 500));
		const edits = await sessions[engine]!.formatting(p.file);
		return { count: edits.length };
	}

	for (const p of probes) {
		// The F# reference must work — a failure here means the rules/setup is
		// broken, not that Rust is behind.
		it(`[fsharp] ${p.name}`, async function () {
			if (p.kind === 'hover') {
				const { actual, missing } = await runHover('fsharp', p);
				assert.ok(actual.length > 0, 'F# reference returned an empty hover — rules not loaded?');
				assert.deepStrictEqual(missing, [], `F# hover missing ${JSON.stringify(missing)}\nactual: ${actual}`);
			} else if (p.kind === 'completion') {
				const { count, missing } = await runCompletion('fsharp', p);
				assert.ok(count > 0, 'F# reference returned no completions');
				assert.deepStrictEqual(missing, [], `F# completion missing ${JSON.stringify(missing)} (got ${count} labels)`);
			} else if (p.kind === 'definition') {
				const { targets } = await runDefinition('fsharp', p);
				assert.ok(targets.length > 0, 'F# reference returned no definition');
				assert.ok(targets.some(t => t === p.expectedTargetFile),
					`F# definition resolved to [${targets}], expected ${p.expectedTargetFile}`);
			} else if (p.kind === 'references') {
				const { count } = await runReferences('fsharp', p);
				assert.ok(count >= p.expectedMinCount,
					`F# found ${count} references, expected >= ${p.expectedMinCount}`);
			} else if (p.kind === 'diagnostics') {
				const { count } = runDiagnostics('fsharp', p);
				if (p.expectAny) {
					assert.ok(count > 0, 'F# reference produced no diagnostics');
				} else {
					assert.deepStrictEqual(count, 0, `F# reference produced ${count} unexpected diagnostics`);
				}
			} else if (p.kind === 'formatting') {
				const { count } = await runFormatting('fsharp', p);
				if (p.expectAny) {
					assert.ok(count > 0, 'F# reference returned no formatting edits');
				} else {
					assert.deepStrictEqual(count, 0, `F# reference returned ${count} unexpected formatting edits`);
				}
			}
		});

		// The Rust port chases the reference. Expected to fail until parity.
		it(`[rust] ${p.name}`, async function () {
			if (p.kind === 'hover') {
				const { actual, missing } = await runHover('rust', p);
				assert.deepStrictEqual(missing, [], `Rust hover missing ${JSON.stringify(missing)}\nactual: ${actual}`);
			} else if (p.kind === 'completion') {
				const { count, missing } = await runCompletion('rust', p);
				assert.deepStrictEqual(missing, [], `Rust completion missing ${JSON.stringify(missing)} (got ${count} labels)`);
			} else if (p.kind === 'definition') {
				const { targets } = await runDefinition('rust', p);
				assert.ok(targets.some(t => t === p.expectedTargetFile),
					`Rust definition resolved to [${targets}], expected ${p.expectedTargetFile}`);
			} else if (p.kind === 'references') {
				const { count } = await runReferences('rust', p);
				assert.ok(count >= p.expectedMinCount,
					`Rust found ${count} references, expected >= ${p.expectedMinCount}`);
			} else if (p.kind === 'diagnostics') {
				const { count } = runDiagnostics('rust', p);
				if (p.expectAny) {
					assert.ok(count > 0, 'Rust produced no diagnostics');
				} else {
					assert.deepStrictEqual(count, 0, `Rust produced ${count} unexpected diagnostics`);
				}
			} else if (p.kind === 'formatting') {
				const { count } = await runFormatting('rust', p);
				if (p.expectAny) {
					assert.ok(count > 0, 'Rust returned no formatting edits');
				} else {
					assert.deepStrictEqual(count, 0, `Rust returned ${count} unexpected formatting edits`);
				}
			}
		});
	}
});
