import * as path from "path";
import { existsSync as fsExistsSync } from "fs";
import * as fsPromises from "fs/promises";
import { workspace, window, ProgressLocation, l10n } from "vscode";
import type { Memento } from "vscode";
import { ExecuteCommandRequest } from "vscode-languageclient/node";
import type { LanguageClient } from "vscode-languageclient/node";
import { serverCommand } from "../common/serverCommandContract";
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
	RULES_MANIFEST_ETAG_CACHE_KEY,
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
	workspaceRoot: string,
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

	const rawManualRules =
		process.env.CWTOOLS_TEST_RULES_FOLDER ??
		workspace.getConfiguration("cwtools").get<string>("rules_folder");
	const manualRules = resolveRulesFolder(rawManualRules, { workspaceRoot });
	const hasManualRules = !!(rawManualRules && rawManualRules.trim() !== "");
	const effectiveRulesCache = manualRules.existed
		? manualRules.path!
		: languageRulesCache;
	if (!manualRules.existed) {
		// LSP configuration only accepts an existing rules cache directory.
		await fsPromises.mkdir(languageRulesCache, { recursive: true });
	}
	if (manualRules.existed) {
		logInfo(`Using manual rules folder: ${manualRules.path}`);
	} else if (hasManualRules) {
		// rules_folder is set but unusable. Warn loudly instead of silently
		// cloning upstream, so the user knows their local rules were ignored.
		logWarn(
			`rules_folder "${rawManualRules}" does not exist (tried "${manualRules.path}"); falling back to bundled/upstream rules.`,
		);
		void window.showWarningMessage(
			l10n.t(
				'CWTools: the rules_folder "{0}" could not be found (tried "{1}"). Falling back to the bundled/upstream rules.',
				rawManualRules,
				manualRules.path!,
			),
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
const inFlightRulesFetches = new WeakMap<LanguageClient, Promise<void>>();

export function fetchRulesInBackground(
	language: string,
	cacheDir: string,
	client: LanguageClient,
	initialScanDone: Promise<void>,
	globalState: Memento,
): Promise<void> {
	if (!LANGUAGE_REPOS[language]) {
		return Promise.resolve();
	}
	const inFlight = inFlightRulesFetches.get(client);
	if (inFlight) return inFlight;

	let fetchPromise = Promise.resolve();
	fetchPromise = syncReviewedRules(
		language,
		cacheDir,
		client,
		initialScanDone,
		globalState,
	)
		.catch((err: unknown) => logError(`Rule fetch failed for ${language}`, err))
		.finally(() => {
			if (inFlightRulesFetches.get(client) === fetchPromise) {
				inFlightRulesFetches.delete(client);
			}
		});
	inFlightRulesFetches.set(client, fetchPromise);
	return fetchPromise;
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

// A stored ETag goes back verbatim as a request header, so only short,
// single-line values are usable; anything else is treated as not stored.
const RULES_MANIFEST_ETAG_MAX_CHARS = 200;

function usableRulesManifestEtag(value: unknown): string | undefined {
	if (
		typeof value !== "string" ||
		value === "" ||
		value.length > RULES_MANIFEST_ETAG_MAX_CHARS ||
		/[\r\n]/.test(value)
	) {
		return undefined;
	}
	return value;
}

// "modified" is the only shape that carries a manifest; "unmodified" (HTTP 304,
// only sent in answer to an If-None-Match we sent) keeps the caller's cache.
type RemoteRulesManifestResult =
	| { kind: "unmodified" }
	| { kind: "modified"; manifest: RulesManifest; etag: string | undefined };

async function fetchRemoteRulesManifest(
	etag: string | undefined,
): Promise<RemoteRulesManifestResult> {
	const controller = new AbortController();
	const timeout = setTimeout(
		() => controller.abort(),
		RULES_MANIFEST_TIMEOUT_MS,
	);
	try {
		const response = await fetch(RULES_MANIFEST_URL, {
			signal: controller.signal,
			headers: etag ? { "If-None-Match": etag } : undefined,
		});
		if (response.status === 304) {
			// 304 is not response.ok, so this must come before the ok check.
			return { kind: "unmodified" };
		}
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
		return {
			kind: "modified",
			manifest: parseRulesManifestText(
				await readRulesManifestBody(response.body),
			),
			etag: usableRulesManifestEtag(response.headers.get("etag")?.trim()),
		};
	} finally {
		clearTimeout(timeout);
	}
}

async function reviewedRulesRepo(
	language: string,
	globalState: Memento,
): Promise<RulesRepo | undefined> {
	const cached = cachedRulesManifest(globalState);
	// If-None-Match is only sent when both sides of the pair are usable; a
	// manifest with no ETag (or the reverse) must always fetch the full body.
	const storedEtag = usableRulesManifestEtag(
		globalState.get(RULES_MANIFEST_ETAG_CACHE_KEY),
	);
	const etag = cached && storedEtag ? storedEtag : undefined;
	let manifest = cached;
	try {
		const fetched = await fetchRemoteRulesManifest(etag);
		if (fetched.kind === "unmodified") {
			if (!cached) {
				throw new Error(
					"Rules manifest is unmodified but no usable manifest is cached.",
				);
			}
			// The cache is still current: keep it and write nothing.
			return rulesRepoForManifest(language, cached);
		}
		manifest = selectRulesManifest(cached, fetched.manifest);
		if (cached && fetched.manifest.revision < cached.revision) {
			logWarn(
				`Ignoring stale rules manifest revision ${fetched.manifest.revision}; cached revision ${cached.revision} is newer.`,
			);
		}
		let manifestCached = manifest === cached;
		if (!manifestCached) {
			try {
				await globalState.update(RULES_MANIFEST_CACHE_KEY, manifest);
				manifestCached = true;
			} catch (err) {
				logWarn(`Could not cache rules manifest: ${errorMessage(err)}`);
			}
		}
		// A 200 without an ETag, or a manifest that could not be cached, clears any
		// stored ETag so the next request fetches the body again instead of
		// assuming an unmodified cache.
		const nextEtag = manifestCached ? fetched.etag : undefined;
		if (nextEtag !== storedEtag) {
			try {
				await globalState.update(RULES_MANIFEST_ETAG_CACHE_KEY, nextEtag);
			} catch (err) {
				logWarn(
					`Could not cache rules manifest ETag: ${errorMessage(err)}`,
				);
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
			title: l10n.t("CWTools: updating {0} rules", language),
		},
		async () => {
			try {
				logInfo(
					isInitialClone
						? `Fetching ${language} rules ${rules.ref} from ${rules.repo} into ${languageRulesCache}`
						: `Replacing ${language} rules ${head} with ${rules.ref} from ${rules.repo} into ${languageRulesCache}`,
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
				if (!isInitialClone) {
					// A shallow cache can't tell newer from just different, so name the
					// change rather than claim a direction.
					void window.showInformationMessage(
						l10n.t(
							"CWTools: {0} rules changed from {1} to {2}. Validation may differ under the new pin.",
							language,
							head,
							rules.ref,
						),
					);
				}
				await initialScanDone;
				// Rules are now on disk; tell the (already running) server to
				// pick them up without a window reload.
				await client
					.sendRequest(ExecuteCommandRequest.type, {
						command: serverCommand("reloadrulesconfig"),
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
					const warning =
						err instanceof GitNotFoundError
							? l10n.t(
									"CWTools needs Git on your PATH to fetch the {0} rules; install Git and reload the window.",
									language,
								)
							: l10n.t(
									"CWTools: failed to download the {0} rules ({1}). Validation will be limited until they can be fetched; check your network and reload the window.",
									language,
									msg,
								);
					void window.showWarningMessage(warning);
				}
			}
		},
	);
}
