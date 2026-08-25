import * as assert from 'assert';
import * as vscode from 'vscode';
import * as path from 'path';
import {
	activate,
	waitForLSP,
	waitForLanguageServer,
	EXTENSION_ID,
	SAMPLE_ROOT,
} from '../support/utils';
import { setupLSPErrorMonitoring, checkForLSPErrors, teardownLSPErrorMonitoring } from '../support/lspErrorMonitor';
import { checkHoverContains } from './hoverChecks';
import { expect } from 'chai';

const testEventFile = path.join(SAMPLE_ROOT, 'events', 'irm.txt');
const testEffectsFile = path.join(SAMPLE_ROOT, 'common', 'scripted_effects', 'irm_scripted_effects.txt');

suite('LSP Hover Tests', function () {
	this.timeout(60000);

	let testDocument: vscode.TextDocument;

	setup(async function () {
		// After activation, not before: the monitor hooks defaultClient's output
		// channel, and defaultClient is only set once activation has built the
		// client, so hooking first leaves the first test of a run unmonitored.
		await activate();
		setupLSPErrorMonitoring();
		const extension = vscode.extensions.getExtension(EXTENSION_ID)!;
		assert.ok(extension?.isActive, 'Extension should be active');

		const uri = vscode.Uri.file(testEventFile);
		const document = await vscode.workspace.openTextDocument(uri);
		await vscode.window.showTextDocument(document);
		const isReady = await waitForLanguageServer(uri);
		if (!isReady) console.warn('Language server not ready, tests may not work as expected');
		await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
	});

	teardown(async function () {
		await vscode.commands.executeCommand('workbench.action.closeAllEditors');
		checkForLSPErrors(this.currentTest?.title || 'unknown test');
	});

	suiteTeardown(() => teardownLSPErrorMonitoring());

	suite('Basic Hover Functionality', function () {
		setup(async function () {
			const uri = vscode.Uri.file(testEventFile);
			testDocument = await vscode.workspace.openTextDocument(uri);
			await vscode.window.showTextDocument(testDocument);
			await waitForLanguageServer(uri);
		});

		teardown(async function () {
			await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
		});

		// `is_country_type` inside `every_country = { limit = { … } }`: the hover
		// names the trigger, carries its rule description, and reports the scope
		// the trigger is evaluated in.
		test('names a trigger, its description and the scope it runs in', async function () {
			await waitForLSP(vscode.Uri.file(testEventFile));
			const required = [
				'`is_country_type`',
				'Checks if the country is a specific type',
				'**Scope**: country',
			];
			const result = await checkHoverContains(testDocument.uri, new vscode.Position(37, 45), required);
			expect(result.missing, `actual hover:\n${result.actual}`).to.deep.equal([]);
		});

		// `is_homeworld` sits inside `solar_system = { … }` in a country_event, so
		// the current scope has moved to the system while ROOT and PREV stay on
		// the country. Losing the Root/Prev lines is the regression this catches.
		test('reports the scope change plus the surviving root and prev scopes', async function () {
			await waitForLSP(vscode.Uri.file(testEventFile));
			const required = [
				"Checks if the planet is its owner's homeworld",
				'**Scope**: galacticobject',
				'**Root**: country',
				'**Prev**: country',
			];
			const result = await checkHoverContains(testDocument.uri, new vscode.Position(15, 20), required);
			expect(result.missing, `actual hover:\n${result.actual}`).to.deep.equal([]);
		});
	});

	suite('Localization Hover', function () {
		test('previews the localisation a pop faction flag resolves to', async function () {
			const uri = vscode.Uri.file(testEffectsFile);
			testDocument = await vscode.workspace.openTextDocument(uri);
			await vscode.window.showTextDocument(testDocument);
			await waitForLSP(uri);
			const result = await checkHoverContains(testDocument.uri, new vscode.Position(36, 70), ['Faction Governance']);
			expect(result.missing, `actual hover:\n${result.actual}`).to.deep.equal([]);
		});
	});

	suite('Error Handling', function () {
		test('should handle invalid positions gracefully', async function () {
			const uri = vscode.Uri.file(testEventFile);
			const document = await vscode.workspace.openTextDocument(uri);
			await vscode.window.showTextDocument(document);
			const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
				'vscode.executeHoverProvider',
				document.uri,
				new vscode.Position(1000, 1000)
			);
			console.log('Hovers for invalid position:', hovers?.length || 0);
			await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
		});
	});

	suite('Performance Tests', function () {
		// Answers from the warm index in ~10 ms. The budget is deliberately far
		// above that: it catches a hover that starts blocking on a re-index
		// (seconds) without failing on a loaded runner's scheduling noise.
		test('answers a hover from the warm index instead of re-indexing', async function () {
			const uri = vscode.Uri.file(testEventFile);
			const document = await vscode.workspace.openTextDocument(uri);
			await vscode.window.showTextDocument(document);
			await waitForLanguageServer(document.uri);
			const start = Date.now();
			await vscode.commands.executeCommand<vscode.Hover[]>(
				'vscode.executeHoverProvider',
				document.uri,
				new vscode.Position(8, 7)
			);
			const duration = Date.now() - start;
			assert.ok(duration < 500, `Hover request should complete within 500 ms, took ${duration}ms`);
			await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
		});
	});
});
