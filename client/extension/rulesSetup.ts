import * as path from "path";
import { existsSync as fsExistsSync } from "fs";
import * as fsPromises from "fs/promises";
import { workspace, window, ProgressLocation } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import { ExecuteCommandRequest } from "vscode-languageserver-protocol";
import { LANGUAGE_REPOS, resolveRulesFolder, runGit } from "./engine";
import { logInfo, logWarn, logError } from "./logger";

export interface RulesSetup {
	rulesCache: string;
	fetchUpstream: boolean;
}

// Resolve the rules cache path (after creating the dir). No network access, so
// activation and the LSP start are never blocked here.
export async function resolveRulesCache(
	language: string,
	cacheDir: string,
): Promise<RulesSetup> {
	const repoPath = LANGUAGE_REPOS[language];
	if (!repoPath) {
		logWarn(
			`No config repository for language "${language}"; rule cloning skipped.`,
		);
	}
	logInfo(`${language} ${repoPath || "(no remote)"}`);

	await fsPromises.mkdir(cacheDir, { recursive: true });
	const languageRulesCache = path.join(cacheDir, language);

	const rawManualRules = workspace
		.getConfiguration("cwtools")
		.get<string>("rules_folder");
	const workspaceRoot = workspace.workspaceFolders?.[0]?.uri.fsPath;
	const manualRules = resolveRulesFolder(rawManualRules, { workspaceRoot });
	const hasManualRules = !!(rawManualRules && rawManualRules.trim() !== "");
	const effectiveRulesCache = manualRules.existed
		? manualRules.path!
		: languageRulesCache;
	if (manualRules.existed) {
		logInfo(`Using manual rules folder: ${manualRules.path}`);
	} else if (hasManualRules) {
		// rules_folder is set but unusable. Warn loudly instead of silently
		// cloning upstream, so the user knows their local rules were ignored.
		logWarn(
			`rules_folder "${rawManualRules}" does not exist (tried "${manualRules.path}"); falling back to bundled/upstream rules.`,
		);
		void window.showWarningMessage(
			`CWTools: the rules_folder "${rawManualRules}" could not be found (tried "${manualRules.path}"). Falling back to the bundled/upstream rules.`,
		);
	}
	return {
		rulesCache: effectiveRulesCache,
		fetchUpstream: !manualRules.existed && !!repoPath,
	};
}

// Clone/pull the rules repo in the background. The server starts without the
// rules (it tolerates a missing/empty rules dir) and we signal it to reload
// once the fetch lands, so activation is never stalled on the network.
export function fetchRulesInBackground(
	language: string,
	cacheDir: string,
	client: LanguageClient,
	initialScanDone: Promise<void>,
): void {
	const repoPath = LANGUAGE_REPOS[language];
	if (!repoPath) {
		return;
	}
	const languageRulesCache = path.join(cacheDir, language);
	const gitDir = path.join(languageRulesCache, ".git");
	const isInitialClone = !fsExistsSync(gitDir);
	void window.withProgress(
		{
			location: ProgressLocation.Window,
			title: `CWTools: updating ${language} rules`,
		},
		async () => {
			try {
				if (isInitialClone) {
					logInfo(`Cloning rules from ${repoPath} into ${languageRulesCache}`);
					await runGit(["clone", "--depth", "1", repoPath, languageRulesCache]);
				} else {
					logInfo(`Fetching latest rules for ${language} ...`);
					await runGit([
						"-C",
						languageRulesCache,
						"pull",
						"--depth=1",
						"--ff-only",
					]);
				}
				await initialScanDone;
				// Rules are now on disk; tell the (already running) server to
				// pick them up without a window reload.
				await client
					.sendRequest(ExecuteCommandRequest.type, {
						command: "reloadrulesconfig",
						arguments: [],
					})
					.catch((err) =>
						logError("Failed to reload rules after background fetch", err),
					);
			} catch (err) {
				const msg = err instanceof Error ? err.message : String(err);
				logError(`Rule fetch failed for ${language}`, msg);
				// A failed initial clone leaves the extension with no rules at all, so
				// warn loudly. A failed pull (rules already present) is only a stale
				// offline refresh and stays log-only.
				if (isInitialClone) {
					void window.showWarningMessage(
						`CWTools: failed to download the ${language} rules (${msg}). Validation will be limited until they can be fetched; check your network and reload the window.`,
					);
				}
			}
		},
	);
}
