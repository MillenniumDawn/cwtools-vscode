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
		setupLSPErrorMonitoring();
		await activate();
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
			await waitForLanguageServer(uri, 10, 100);
		});

		teardown(async function () {
			await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
		});

		test('should provide hover information with scope change - effect', async function () {
			await waitForLSP(vscode.Uri.file(testEventFile));
			const required = [
				"Checks if the country is a specific type",
				"Any",
				"Country",
				"ROOT",
				"THIS",
			];
			const result = await checkHoverContains(testDocument.uri, new vscode.Position(37, 45), required);
			console.warn(`[rust] hover gap: missing ${JSON.stringify(result.missing)} in:\n${result.actual}`);
			expect(result.actual, `engine=rust`).to.not.be.empty;
			expect(result.missing, `engine=rust\nactual: ${result.actual}`).to.deep.equal([]);
		});

		test('should provide hover information with scope change - trigger', async function () {
			await waitForLSP(vscode.Uri.file(testEventFile));
			const required = [
				"Checks if the planet is its owner's homeworld",
				"System",
				"Country",
				"ROOT",
				"THIS",
				"PREV",
			];
			const result = await checkHoverContains(testDocument.uri, new vscode.Position(15, 20), required);
			console.warn(`[rust] hover gap: missing ${JSON.stringify(result.missing)} in:\n${result.actual}`);
			expect(result.actual, `engine=rust`).to.not.be.empty;
			expect(result.missing, `engine=rust\nactual: ${result.actual}`).to.deep.equal([]);
		});
	});

	suite('Localization Hover', function () {
		test('should provide localization information in hover', async function () {
			const uri = vscode.Uri.file(testEffectsFile);
			testDocument = await vscode.workspace.openTextDocument(uri);
			await vscode.window.showTextDocument(testDocument);
			await waitForLSP(vscode.Uri.file(testEffectsFile));
			const result = await checkHoverContains(testDocument.uri, new vscode.Position(36, 70), ["Faction Governance"]);
			console.warn(`[rust] localization hover gap: missing ${JSON.stringify(result.missing)} in:\n${result.actual}`);
			expect(result.actual, `engine=rust`).to.not.be.empty;
			expect(result.missing, `engine=rust\nactual: ${result.actual}`).to.deep.equal([]);
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
		test('should respond to hover requests within reasonable time', async function () {
			const uri = vscode.Uri.file(testEventFile);
			const document = await vscode.workspace.openTextDocument(uri);
			await vscode.window.showTextDocument(document);
			await waitForLanguageServer(document.uri, 10, 100);
			const start = Date.now();
			const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
				'vscode.executeHoverProvider',
				document.uri,
				new vscode.Position(8, 7)
			);
			const duration = Date.now() - start;
			console.log(`Hover request took ${duration}ms`);
			assert.ok(duration < 100, `Hover request should complete within 100 ms, took ${duration}ms`);
			if (hovers) console.log('Performance test - hovers found:', hovers.length);
			await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
		});
	});
});
