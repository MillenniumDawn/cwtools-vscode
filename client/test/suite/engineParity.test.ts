/**
 * Engine parity tests. These are run separately for each engine:
 *   npm run test:engine:fsharp   (sets cwtools.engine=fsharp, runs mocha)
 *   npm run test:engine:rust     (sets cwtools.engine=rust,   runs mocha)
 *
 * The strict F# assertions in hover.test.ts and completion.test.ts already
 * fail under the Rust engine when it lacks parity, which is the right
 * loud-failure signal. This suite adds a structured parity report: each
 * check returns a result object (passed/actual/missing) that we record
 * here, and the suite's `after` hook writes a summary so the gap is
 * visible even when individual assertions are run in the matching engine.
 */
import * as assert from 'assert';
import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import {
	activate,
	waitForLSP,
	waitForLanguageServer,
	currentEngine,
	openDocumentAndShow,
} from '../utils';
import {
	setupLSPErrorMonitoring,
	teardownLSPErrorMonitoring,
} from '../lspErrorMonitor';
import { checkHoverContains, getCompletionLabels } from './hoverChecks';

const sampleRoot = path.resolve(__dirname, '../sample');
const testEventFile = path.join(sampleRoot, 'events', 'irm.txt');
const testEffectsFile = path.join(sampleRoot, 'common', 'scripted_effects', 'irm_scripted_effects.txt');
const testNicheFile = path.join(sampleRoot, 'common', 'pop_faction_types', 'irm_regionalist.txt');

interface ParityCheck {
	name: string;
	engine: string;
	passed: boolean;
	actual?: string;
	missing?: string[];
	error?: string;
}

const results: ParityCheck[] = [];

// Apply the engine pin before the extension activates. Reading the
// setting from disk and writing the forced value is async, but VS Code
// has already loaded the user config by the time this module runs, so
// we can fire-and-forget the update. The setup() await's it.
async function applyEngineOverride(): Promise<void> {
	const forced = process.env.CWTOOLS_ENGINE;
	if (forced === 'fsharp' || forced === 'rust') {
		await vscode.workspace.getConfiguration('cwtools').update(
			'engine', forced, vscode.ConfigurationTarget.Global
		);
	}
}

function record(name: string, passed: boolean, detail: Partial<ParityCheck> = {}): void {
	results.push({ name, engine: currentEngine(), passed, ...detail });
}

suite('Engine parity — strict F# feature checks', function () {
	this.timeout(90000);

	setup(async function () {
		await applyEngineOverride();
		setupLSPErrorMonitoring();
		await activate();
		const eventUri = vscode.Uri.file(testEventFile);
		await openDocumentAndShow(eventUri);
		const ready = await waitForLanguageServer(eventUri);
		assert.ok(ready, 'LSP must be ready for parity checks');
	});

	suiteTeardown(async function () {
		teardownLSPErrorMonitoring();
		await vscode.commands.executeCommand('workbench.action.closeAllEditors');

		// Write a parity report to the test output directory so the gap is
		// visible from CI logs even when this test passes.
		const reportPath = path.resolve(__dirname, `parity-${currentEngine()}.json`);
		const passed = results.filter(r => r.passed).length;
		const failed = results.filter(r => !r.passed);
		const report = {
			engine: currentEngine(),
			timestamp: new Date().toISOString(),
			total: results.length,
			passed,
			failed: failed.length,
			failing: failed,
		};
		try {
			fs.writeFileSync(reportPath, JSON.stringify(report, null, 2));
			console.log(`[parity] Wrote ${reportPath}: ${passed}/${results.length} passed`);
		} catch (e) {
			console.warn('[parity] Failed to write report:', e);
		}
	});

	test('hover: country_type field documents what it checks', async function () {
		await waitForLSP(vscode.Uri.file(testEventFile));
		const result = await checkHoverContains(
			vscode.Uri.file(testEventFile),
			new vscode.Position(37, 45),
			[
				"Checks if the country is a specific type",
				"Any",
				"Country",
				"ROOT",
				"THIS",
			]
		);
		record('hover country_type', result.passed, { actual: result.actual, missing: result.missing });
		assert.ok(result.passed, `engine=${currentEngine()}\nactual: ${result.actual}\nmissing: ${JSON.stringify(result.missing)}`);
	});

	test('hover: is_homeworld trigger documents scope chain', async function () {
		await waitForLSP(vscode.Uri.file(testEventFile));
		const result = await checkHoverContains(
			vscode.Uri.file(testEventFile),
			new vscode.Position(15, 20),
			[
				"Checks if the planet is its owner's homeworld",
				"System",
				"Country",
				"ROOT",
				"THIS",
				"PREV",
			]
		);
		record('hover is_homeworld', result.passed, { actual: result.actual, missing: result.missing });
		assert.ok(result.passed, `engine=${currentEngine()}\nactual: ${result.actual}\nmissing: ${JSON.stringify(result.missing)}`);
	});

	test('hover: localization key shows the localized string', async function () {
		const effectsUri = vscode.Uri.file(testEffectsFile);
		await openDocumentAndShow(effectsUri);
		await waitForLSP(effectsUri);
		const result = await checkHoverContains(
			effectsUri,
			new vscode.Position(36, 70),
			["Faction Governance"]
		);
		record('hover localization', result.passed, { actual: result.actual, missing: result.missing });
		assert.ok(result.passed, `engine=${currentEngine()}\nactual: ${result.actual}\nmissing: ${JSON.stringify(result.missing)}`);
	});

	test('completion: niche context returns the expected scripted effect labels', async function () {
		const nicheUri = vscode.Uri.file(testNicheFile);
		await openDocumentAndShow(nicheUri);
		await waitForLSP(nicheUri);
		const labels = await getCompletionLabels(nicheUri, new vscode.Position(26, 41));
		const required = ["regionalist_dublicated", "sector_policy_leadership"];
		const missing = required.filter(e => !labels.includes(e));
		const passed = missing.length === 0;
		record('completion niche', passed, {
			actual: `${labels.length} items`,
			missing,
		});
		assert.ok(passed, `engine=${currentEngine()}\ngot ${labels.length} labels, missing ${JSON.stringify(missing)}`);
	});
});
