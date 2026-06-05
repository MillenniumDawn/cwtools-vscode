import * as assert from 'assert';
import * as vscode from 'vscode';
import * as path from 'path';
import { activate, waitForLSP, currentEngine } from '../utils';
import { setupLSPErrorMonitoring, checkForLSPErrors, teardownLSPErrorMonitoring } from '../lspErrorMonitor';
import { getCompletionLabels } from './hoverChecks';
import { expect } from 'chai';

const sampleRoot = path.resolve(__dirname, '../sample');
const testEventFile = path.join(sampleRoot, 'events', 'irm.txt');
const testNicheFile = path.join(sampleRoot, 'common', 'pop_faction_types', 'irm_regionalist.txt');

async function getCompletions(uri: vscode.Uri, position: vscode.Position): Promise<vscode.CompletionList> {
	const completions = await vscode.commands.executeCommand<vscode.CompletionList>(
		'vscode.executeCompletionItemProvider',
		uri,
		position
	);
	assert.ok(completions?.items?.length, 'No completions received');
	const textTypeCount = completions.items.filter(item => (item.kind || 0) === 0).length;
	assert.ok(textTypeCount == 0,
		`Too many Text type completions (${textTypeCount}/${completions.items.length}) - LSP may not be working`);
	return completions;
}

async function openAndShow(file: string): Promise<vscode.TextDocument> {
	const uri = vscode.Uri.file(file);
	const document = await vscode.workspace.openTextDocument(uri);
	await vscode.window.showTextDocument(document);
	return document;
}

suite('LSP Completion Tests', function () {
	this.timeout(60000);

	setup(async function () {
		setupLSPErrorMonitoring();
		await activate();
		const extension = vscode.extensions.getExtension('tboby.cwtools-vscode')!;
		assert.ok(extension?.isActive, 'Extension should be active');
		const document = await openAndShow(testEventFile);
		await waitForLSP(document.uri);
	});

	teardown(async function () {
		await vscode.commands.executeCommand('workbench.action.closeAllEditors');
		checkForLSPErrors(this.currentTest?.title || 'unknown test');
	});

	suiteTeardown(() => teardownLSPErrorMonitoring());

	test('should provide completions in niche context', async function () {
		const document = await openAndShow(testNicheFile);
		const completions = await getCompletions(document.uri, new vscode.Position(26, 41));
		const labels = completions.items.map(item =>
			typeof item.label === 'string' ? item.label : item.label.label
		);
		const expected = ["regionalist_dublicated", "sector_policy_leadership"];
		const missing = expected.filter(e => !labels.includes(e));
		if (currentEngine() === 'rust' && missing.length > 0) {
			console.warn(`[rust] niche completion gap: missing ${JSON.stringify(missing)} in ${labels.length} items`);
		}
		expect(missing, `engine=${currentEngine()}\ngot ${labels.length} labels, missing: ${JSON.stringify(missing)}`).to.deep.equal([]);
	});

	test('should provide completions in trigger context', async function () {
		const document = await openAndShow(testEventFile);
		const completions = await getCompletions(document.uri, new vscode.Position(12, 0));
		const labels = completions.items.map(item =>
			typeof item.label === 'string' ? item.label : item.label.label
		);
		const hasRelevantTriggers = labels.some(label =>
			label.includes('is_ai') || label.includes('limit') || label.includes('country_type')
		);
		assert.ok(hasRelevantTriggers);
		assert.ok(completions.items.length > 0, 'Should have completion items');
	});

	test('should provide completions in effect context', async function () {
		const document = await openAndShow(testEventFile);
		const completions = await getCompletions(document.uri, new vscode.Position(17, 8));
		const labels = completions.items.map(item =>
			typeof item.label === 'string' ? item.label : item.label.label
		);
		assert.ok(labels.length > 0);
		assert.ok(completions.items.length > 0, 'Should have completion items in effect context');
	});

	test('should respond to completion requests quickly', async function () {
		const document = await openAndShow(testEventFile);
		const start = Date.now();
		const completions = await getCompletions(document.uri, new vscode.Position(12, 0));
		const duration = Date.now() - start;
		assert.ok(duration < 5000, `Completion should be fast, took ${duration}ms`);
		assert.ok(completions.items.length > 0, 'Should have completion items');
	});

	test('should provide LSP-based completions not just text fallback', async function () {
		const document = await openAndShow(testEventFile);
		const completions = await getCompletions(document.uri, new vscode.Position(12, 0));
		const hasLSPFeatures = completions.items.some(item =>
			item.detail || item.documentation || item.sortText ||
			(item.commitCharacters && item.commitCharacters.length > 0)
		);
		assert.ok(hasLSPFeatures, 'Completions should have LSP-specific features like detail, documentation, or sortText');
	});
});
