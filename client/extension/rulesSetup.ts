import * as path from 'path';
import { existsSync as fsExistsSync } from 'fs';
import * as fsPromises from 'fs/promises';
import { workspace, window, ProgressLocation } from 'vscode';
import { LANGUAGE_REPOS, resolveRulesFolder, runGit } from './engine';
import { logInfo, logWarn, logError } from './logger';

export interface RulesSetup {
	rulesCache: string;
}

export async function ensureRules(language: string, cacheDir: string): Promise<RulesSetup> {
	const repoPath = LANGUAGE_REPOS[language];
	if (!repoPath) {
		logWarn(`No config repository for language "${language}"; rule cloning skipped.`);
	}
	logInfo(`${language} ${repoPath || '(no remote)'}`);

	await fsPromises.mkdir(cacheDir, { recursive: true });
	const languageRulesCache = path.join(cacheDir, language);

	const rawManualRules = workspace.getConfiguration('cwtools').get<string>('rules_folder');
	const workspaceRoot = workspace.workspaceFolders?.[0]?.uri.fsPath;
	const manualRules = resolveRulesFolder(rawManualRules, { workspaceRoot });
	const hasManualRules = !!(rawManualRules && rawManualRules.trim() !== '');
	const effectiveRulesCache = manualRules.existed ? manualRules.path! : languageRulesCache;
	if (manualRules.existed) {
		logInfo(`Using manual rules folder: ${manualRules.path}`);
	} else if (hasManualRules) {
		// rules_folder is set but unusable. Warn loudly instead of silently
		// cloning upstream, so the user knows their local rules were ignored.
		logWarn(`rules_folder "${rawManualRules}" does not exist (tried "${manualRules.path}"); falling back to bundled/upstream rules.`);
		void window.showWarningMessage(
			`CWTools: the rules_folder "${rawManualRules}" could not be found (tried "${manualRules.path}"). Falling back to the bundled/upstream rules.`
		);
	}
	if (!manualRules.existed && repoPath) {
		const gitDir = path.join(languageRulesCache, '.git');
		const isInitialClone = !fsExistsSync(gitDir);
		try {
			// Status-bar spinner: the clone/pull runs during activation and can
			// take up to 60s on a slow network, previously with no UI at all.
			await window.withProgress(
				{ location: ProgressLocation.Window, title: `CWTools: updating ${language} rules` },
				async () => {
					if (isInitialClone) {
						logInfo(`Cloning rules from ${repoPath} into ${languageRulesCache}`);
						await runGit(['clone', '--depth', '1', repoPath, languageRulesCache]);
					} else {
						logInfo(`Fetching latest rules for ${language} ...`);
						await runGit(['-C', languageRulesCache, 'pull', '--depth=1', '--ff-only']);
					}
				}
			);
		} catch (err) {
			const msg = err instanceof Error ? err.message : String(err);
			logError(`Rule fetch failed for ${language}`, msg);
			// A failed initial clone leaves the extension with no rules at all, so
			// warn loudly. A failed pull (rules already present) is only a stale
			// offline refresh and stays log-only.
			if (isInitialClone) {
				void window.showWarningMessage(
					`CWTools: failed to download the ${language} rules (${msg}). Validation will be limited until they can be fetched; check your network and reload the window.`
				);
			}
		}
	}

	return { rulesCache: effectiveRulesCache };
}
