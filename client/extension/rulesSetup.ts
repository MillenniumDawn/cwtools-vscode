import * as path from "path";
import { existsSync as fsExistsSync } from "fs";
import * as fsPromises from "fs/promises";
import { workspace, window, ProgressLocation } from "vscode";
import type { Memento } from "vscode";
import { ExecuteCommandRequest } from "vscode-languageclient/node";
import type { LanguageClient } from "vscode-languageclient/node";
import {
	LANGUAGE_REPOS,
	resolveRulesFolder,
	runGit,
	rulesFetchCommands,
	GitNotFoundError,
} from "./engine";
import type { RulesRepo } from "./engine";
import {
	parseRulesManifest,
	parseRulesManifestText,
	readRulesManifestBody,
	RULES_MANIFEST_CACHE_KEY,
	RULES_MANIFEST_MAX_BYTES,
	RULES_MANIFEST_TIMEOUT_MS,
	RULES_MANIFEST_URL,
	rulesRepoForManifest,
	selectRulesManifest,
	type RulesManifest,
} from "./rulesManifest";
import { logInfo, logWarn, logError, errorMessage } from "./logger";

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
	const rules = LANGUAGE_REPOS[language];
	if (!rules) {
		logWarn(
			`No config repository for language "${language}"; rule cloning skipped.`,
		);
	}
	logInfo(
		`${language} ${rules ? `${rules.repo}@${rules.ref}` : "(no remote)"}`,
	);

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
		fetchUpstream: !manualRules.existed && !!rules,
	};
}

// Fetch the pinned rules commit in the background. The server starts without
// the rules (it tolerates a missing/empty rules dir) and we signal it to reload
// once the fetch lands, so activation is never stalled on the network.
export function fetchRulesInBackground(
	language: string,
	cacheDir: string,
	client: LanguageClient,
	initialScanDone: Promise<void>,
	globalState: Memento,
): void {
	if (!LANGUAGE_REPOS[language]) {
		return;
	}
	void syncReviewedRules(
		language,
		cacheDir,
		client,
		initialScanDone,
		globalState,
	).catch((err: unknown) => logError(`Rule fetch failed for ${language}`, err));
}

async function syncReviewedRules(
	language: string,
	cacheDir: string,
	client: LanguageClient,
	initialScanDone: Promise<void>,
	globalState: Memento,
): Promise<void> {
	const rules = await reviewedRulesRepo(language, globalState);
	if (!rules) {
		return;
	}
	await syncPinnedRules(rules, language, cacheDir, client, initialScanDone);
}

function cachedRulesManifest(globalState: Memento): RulesManifest | undefined {
	const cached = globalState.get<unknown>(RULES_MANIFEST_CACHE_KEY);
	if (cached === undefined) {
		return undefined;
	}
	try {
		return parseRulesManifest(cached);
	} catch (err) {
		logWarn(`Ignoring cached rules manifest: ${errorMessage(err)}`);
		return undefined;
	}
}

async function fetchRemoteRulesManifest(): Promise<RulesManifest> {
	const controller = new AbortController();
	const timeout = setTimeout(
		() => controller.abort(),
		RULES_MANIFEST_TIMEOUT_MS,
	);
	try {
		const response = await fetch(RULES_MANIFEST_URL, {
			signal: controller.signal,
		});
		if (!response.ok) {
			throw new Error(`Rules manifest request failed with ${response.status}.`);
		}
		const contentLength = Number(response.headers.get("content-length"));
		if (
			Number.isFinite(contentLength) &&
			contentLength > RULES_MANIFEST_MAX_BYTES
		) {
			throw new Error("Rules manifest response is too large.");
		}
		return parseRulesManifestText(await readRulesManifestBody(response.body));
	} finally {
		clearTimeout(timeout);
	}
}

async function reviewedRulesRepo(
	language: string,
	globalState: Memento,
): Promise<RulesRepo | undefined> {
	const cached = cachedRulesManifest(globalState);
	let manifest = cached;
	try {
		const fetched = await fetchRemoteRulesManifest();
		manifest = selectRulesManifest(cached, fetched);
		if (cached && fetched.revision < cached.revision) {
			logWarn(
				`Ignoring stale rules manifest revision ${fetched.revision}; cached revision ${cached.revision} is newer.`,
			);
		}
		if (manifest !== cached) {
			try {
				await globalState.update(RULES_MANIFEST_CACHE_KEY, manifest);
			} catch (err) {
				logWarn(`Could not cache rules manifest: ${errorMessage(err)}`);
			}
		}
	} catch (err) {
		logWarn(
			`Rules manifest refresh failed; using ${cached ? "cached" : "bundled"} pins: ${errorMessage(err)}`,
		);
	}
	return rulesRepoForManifest(language, manifest);
}

// The commit the cached rules sit on, or null when there's nothing usable
// there, including a half-written clone that the next fetch rebuilds.
async function currentRulesHead(dir: string): Promise<string | null> {
	if (!fsExistsSync(path.join(dir, ".git"))) {
		return null;
	}
	try {
		return (await runGit(["-C", dir, "rev-parse", "HEAD"])).trim();
	} catch {
		return null;
	}
}

async function syncPinnedRules(
	rules: RulesRepo,
	language: string,
	cacheDir: string,
	client: LanguageClient,
	initialScanDone: Promise<void>,
): Promise<void> {
	const languageRulesCache = path.join(cacheDir, language);
	const head = await currentRulesHead(languageRulesCache);
	const commands = rulesFetchCommands(languageRulesCache, rules, head);
	if (commands.length === 0) {
		logInfo(`${language} rules already at ${rules.ref}.`);
		return;
	}
	const isInitialClone = head === null;
	await window.withProgress(
		{
			location: ProgressLocation.Window,
			title: `CWTools: updating ${language} rules`,
		},
		async () => {
			try {
				logInfo(
					`Fetching ${language} rules ${rules.ref} from ${rules.repo} into ${languageRulesCache}`,
				);
				for (const args of commands) {
					await runGit(args);
				}
				const fetchedHead = await currentRulesHead(languageRulesCache);
				if (fetchedHead !== rules.ref) {
					throw new Error(
						`Rules cache landed on ${fetchedHead ?? "no commit"}, not ${rules.ref}.`,
					);
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
				const msg = errorMessage(err);
				logError(`Rule fetch failed for ${language}`, msg);
				// A failed initial clone leaves the extension with no rules at all, so
				// warn loudly. A failed bump (rules already present) just keeps the
				// previous pin and stays log-only.
				if (isInitialClone) {
					// Missing-git is a setup problem, not a network one; point the user at
				// the actual fix instead of the generic fetch-failure wording.
					const warning = err instanceof GitNotFoundError
						? `CWTools needs Git on your PATH to fetch the ${language} rules; install Git and reload the window.`
						: `CWTools: failed to download the ${language} rules (${msg}). Validation will be limited until they can be fetched; check your network and reload the window.`;
					void window.showWarningMessage(warning);
				}
			}
		},
	);
}
