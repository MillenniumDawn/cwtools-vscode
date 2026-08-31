import * as assert from 'assert';
import * as vscode from 'vscode';
import * as path from 'path';
import { activate, waitForLSP, EXTENSION_ID, openDocumentAndShow, SAMPLE_ROOT, extractCompletionLabel } from '../support/utils';
import { setupLSPErrorMonitoring, checkForLSPErrors, teardownLSPErrorMonitoring } from '../support/lspErrorMonitor';
import { expect } from 'chai';

const testEventFile = path.join(SAMPLE_ROOT, 'events', 'irm.txt');
const testNicheFile = path.join(SAMPLE_ROOT, 'common', 'pop_faction_types', 'irm_regionalist.txt');

async function getCompletionLabels(uri: vscode.Uri, position: vscode.Position): Promise<string[]> {
	const completions = await vscode.commands.executeCommand<vscode.CompletionList>(
		'vscode.executeCompletionItemProvider',
		uri,
		position
	);
	assert.ok(completions?.items?.length, 'No completions received');
	// Kind 0 is Text, the word list VS Code falls back to with no server behind
	// it. Any of those means the assertions below would be reading the fallback.
	const textTypeCount = completions.items.filter(item => (item.kind || 0) === 0).length;
	assert.ok(textTypeCount === 0,
		`Too many Text type completions (${textTypeCount}/${completions.items.length}) - LSP may not be working`);
	return completions.items.map(item => extractCompletionLabel(item));
}

suite('LSP Completion Tests', function () {
	this.timeout(60000);

	setup(async function () {
		// Activation exposes the running client's output channel before the
		// monitor hooks it.
		await activate();
		await setupLSPErrorMonitoring();
		const extension = vscode.extensions.getExtension(EXTENSION_ID)!;
		assert.ok(extension?.isActive, 'Extension should be active');
		const document = await openDocumentAndShow(vscode.Uri.file(testEventFile));
		await waitForLSP(document.uri);
	});

	teardown(async function () {
		await vscode.commands.executeCommand('workbench.action.closeAllEditors');
		checkForLSPErrors(this.currentTest?.title || 'unknown test');
	});

	suiteTeardown(() => teardownLSPErrorMonitoring());

	// The two context tests are a pair: each asserts the names it must offer and
	// the names from the opposite context it must not. Losing scope awareness
	// makes one list bleed into the other, which a "returned some items" check
	// would not notice.
	test('offers triggers and withholds effects inside a trigger block', async function () {
		const document = await openDocumentAndShow(vscode.Uri.file(testEventFile));
		const labels = await getCompletionLabels(document.uri, new vscode.Position(12, 0));
		expect(labels).to.include.members(['is_ai', 'is_country_type']);
		for (const label of ['country_event', 'set_country_flag']) {
			expect(labels).to.not.include.members([label]);
		}
	});

	test('offers effects and withholds triggers inside an immediate block', async function () {
		const document = await openDocumentAndShow(vscode.Uri.file(testEventFile));
		const labels = await getCompletionLabels(document.uri, new vscode.Position(36, 2));
		expect(labels).to.include.members(['country_event', 'every_country', 'set_country_flag']);
		for (const label of ['is_ai', 'is_country_type']) {
			expect(labels).to.not.include.members([label]);
		}
	});

	// MillenniumDawn/cwtools#318 diagnosed this as a per-file value set, but the
	// index is workspace-wide; the real bug was requesting completion mid-token
	// ("regiona"), whose subsequence filter drops sector_policy_leadership. An
	// empty-token position admits everything, so it proves the cross-file merge:
	// regionalist_dublicated (set in this file) and sector_policy_leadership (set
	// in common/button_effects) must both come back.
	test('offers a pop faction flag set in another file', async function () {
		const document = await openDocumentAndShow(vscode.Uri.file(testNicheFile));
		const labels = await getCompletionLabels(document.uri, new vscode.Position(26, 34));
		expect(labels).to.include.members(['regionalist_dublicated', 'sector_policy_leadership']);
	});
});
