import { existsSync } from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { commands, env, l10n, window, workspace } from "vscode";
import type { ExtensionContext, WorkspaceFolder } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import { serverCommand } from "../common/serverCommandContract";
import type { GameDetection } from "./detectGame";
import { runGit } from "./engine";
import { currentRulesHead, readRulesFolderSetting } from "./rulesSetup";
import {
	renderSetupReport,
	SETUP_REPORT_SCHEME,
	type ClientFacts,
	type ServerOutcome,
} from "./setupReportText";
import { errorMessage } from "./logger";

export interface SetupReportContext {
	language: string;
	detectedBy: GameDetection["source"];
	serverExe: string | undefined;
	cacheDir: string;
	workspaceFolder: WorkspaceFolder;
}

async function fetchServerReport(
	client: LanguageClient | undefined,
): Promise<ServerOutcome> {
	if (!client?.isRunning()) {
		return {
			kind: "unavailable",
			reason: "the language server is not running or is still starting",
		};
	}
	try {
		const result: unknown = await commands.executeCommand(
			serverCommand("getSetupReport"),
		);
		if (result !== null && typeof result === "object") {
			return { kind: "report", report: result };
		}
		return { kind: "unavailable", reason: "the server returned no report" };
	} catch (err) {
		return { kind: "unavailable", reason: errorMessage(err) };
	}
}

// A rules_folder is often a subfolder of a checkout (its Config directory), which
// the managed clone's own ".git must sit here" check would call "not a checkout".
async function gitHead(dir: string): Promise<string | null> {
	try {
		return (await runGit(["-C", dir, "rev-parse", "HEAD"])).trim();
	} catch {
		return null;
	}
}

async function clientFacts(
	context: ExtensionContext,
	setup: SetupReportContext,
): Promise<ClientFacts> {
	const { setting, resolved } = readRulesFolderSetting(
		setup.workspaceFolder.uri.fsPath,
	);
	const managedDir = path.join(setup.cacheDir, setup.language);
	const dir = resolved.existed ? resolved.path! : managedDir;
	const vanilla = workspace
		.getConfiguration("cwtools")
		.get<string>(`cache.${setup.language}`)
		?.trim();
	return {
		extensionVersion:
			(context.extension.packageJSON as { version?: string }).version ??
			"unknown",
		vscodeVersion: vscode.version,
		language: setup.language,
		detectedBy: setup.detectedBy,
		serverExe: setup.serverExe,
		rules: {
			setting,
			settingPath: resolved.path,
			settingExists: resolved.existed,
			managedDir,
			dir,
			head: resolved.existed
				? await gitHead(dir)
				: await currentRulesHead(dir),
		},
		vanillaSetting: vanilla || undefined,
		vanillaSettingExists: vanilla ? existsSync(vanilla) : false,
	};
}

// openTextDocument hands back a document that is already open without waiting
// for the provider's refresh, so wait (bounded) for the new text to arrive.
async function untilShows(
	document: vscode.TextDocument,
	text: string,
): Promise<void> {
	if (document.getText() === text) {
		return;
	}
	await new Promise<void>((resolve) => {
		const finish = () => {
			clearTimeout(timer);
			listener.dispose();
			resolve();
		};
		const timer = setTimeout(finish, 2000);
		const listener = workspace.onDidChangeTextDocument((event) => {
			if (event.document === document && document.getText() === text) {
				finish();
			}
		});
	});
}

// Registered before the server binary is looked up, so a setup that cannot start
// a server can still be described.
export function registerSetupReport(
	context: ExtensionContext,
	getClient: () => LanguageClient | undefined,
	setup: SetupReportContext,
): void {
	const uri = vscode.Uri.from({
		scheme: SETUP_REPORT_SCHEME,
		path: "/CWTools setup report.md",
	});
	let text: string | undefined;
	const build = async (): Promise<string> => {
		text = renderSetupReport(
			await clientFacts(context, setup),
			await fetchServerReport(getClient()),
		);
		return text;
	};
	const changed = new vscode.EventEmitter<vscode.Uri>();
	context.subscriptions.push(
		changed,
		// A restored tab asks for its content before the command has run.
		workspace.registerTextDocumentContentProvider(SETUP_REPORT_SCHEME, {
			onDidChange: changed.event,
			provideTextDocumentContent: () => text ?? build(),
		}),
		commands.registerCommand("cwtools.showSetupReport", async () => {
			const fresh = await build();
			changed.fire(uri);
			const document = await workspace.openTextDocument(uri);
			await untilShows(document, fresh);
			await window.showTextDocument(document, { preview: false });
		}),
		commands.registerCommand("cwtools.copySetupReport", async () => {
			const document = workspace.textDocuments.find(
				(open) => open.uri.scheme === SETUP_REPORT_SCHEME,
			);
			if (!document) {
				return;
			}
			await env.clipboard.writeText(document.getText());
			window.setStatusBarMessage(
				l10n.t("CWTools: setup report copied to the clipboard."),
				3000,
			);
		}),
	);
}
