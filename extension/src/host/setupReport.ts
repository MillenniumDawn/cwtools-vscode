import { existsSync } from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { commands, env, l10n, window, workspace } from "vscode";
import type { ExtensionContext, WorkspaceFolder } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import { serverCommand } from "../common/serverCommandContract";
import type { GameDetection } from "./detectGame";
import { runGit } from "./engine";
import { readRulesFolderSetting } from "./rulesSetup";
import {
	renderSetupReport,
	SETUP_REPORT_SCHEME,
	type ClientFacts,
	type ServerOutcome,
	type ServerSetupReport,
} from "./setupReportText";
import { errorMessage } from "./logger";

interface SetupReportContext {
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
			return { kind: "report", report: result as ServerSetupReport };
		}
		return { kind: "unavailable", reason: "the server returned no report" };
	} catch (err) {
		return { kind: "unavailable", reason: errorMessage(err) };
	}
}

// With -C, since a rules_folder is often a subfolder of a checkout (its Config directory).
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
	const managedDir = path.join(setup.cacheDir, setup.language);
	const { setting, resolved, dir } = readRulesFolderSetting(
		setup.workspaceFolder.uri.fsPath,
		managedDir,
	);
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
			head: await gitHead(dir),
		},
		vanillaSetting: vanilla || undefined,
		vanillaSettingExists: vanilla ? existsSync(vanilla) : false,
	};
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
	const changed = new vscode.EventEmitter<vscode.Uri>();
	context.subscriptions.push(
		changed,
		workspace.registerTextDocumentContentProvider(SETUP_REPORT_SCHEME, {
			onDidChange: changed.event,
			provideTextDocumentContent: async () =>
				renderSetupReport(
					await clientFacts(context, setup),
					await fetchServerReport(getClient()),
				),
		}),
		commands.registerCommand("cwtools.showSetupReport", async () => {
			// An open report refreshes when the provider answers, just after it shows.
			changed.fire(uri);
			const document = await workspace.openTextDocument(uri);
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
