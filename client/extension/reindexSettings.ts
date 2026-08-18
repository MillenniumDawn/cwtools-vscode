// Pure helpers for the background-reindex and ignore settings, split out from
// lspClient.ts (which imports vscode) so the node unit tests can exercise them.

// The server reads remapped keys (ignoreFilePatterns/ignoredErrorCodes); the
// `cwtools.*` settings use different names. Map them here so both the initial
// initializationOptions and the live didChangeConfiguration payload agree.
// ignore_patterns are already globs; errors.ignorefiles lists bare file names
// or globs, which the server now matches at any depth on its own (a pattern
// with no path separator matches the name regardless of where it sits).
export function mapIgnoreOptions(
	ignorePatterns: readonly string[] | undefined,
	ignoreFiles: readonly string[] | undefined,
	ignoredCodes: readonly string[] | undefined,
): { ignoreFilePatterns: string[]; ignoredErrorCodes: string[] } {
	return {
		ignoreFilePatterns: [...(ignorePatterns ?? []), ...(ignoreFiles ?? [])],
		ignoredErrorCodes: [...(ignoredCodes ?? [])],
	};
}

// Minutes between the server's periodic background re-index passes. An unset
// setting (undefined) falls back to 30; an explicit 0 disables the loop and is
// preserved, as are the user's negative/fractional values (the server clamps).
export function normalizeBackgroundReindexMinutes(
	value: number | undefined,
): number {
	return value ?? 30;
}

// Seconds of user inactivity before a background pass is allowed to start, so
// a rescan never competes with a request the user is waiting on. Unset falls
// back to the server's own default of 15.
export function normalizeBackgroundReindexIdleSeconds(
	value: number | undefined,
): number {
	return value ?? 15;
}

export type HoverScopeDisplay = "context" | "resolved";

export interface LiveServerSettings {
	localisationLanguages: string[];
	hoverShowAllLanguages: boolean;
	hoverDebug: boolean;
	hoverScopeDisplay: HoverScopeDisplay;
}

export const LIVE_SETTINGS_KEYS = [
	"cwtools.errors.ignore",
	"cwtools.errors.ignorefiles",
	"cwtools.ignore_patterns",
	"cwtools.backgroundReindex.intervalMinutes",
	"cwtools.backgroundReindex.idleSeconds",
	"cwtools.localisation.languages",
	"cwtools.localisation.hoverShowAllLanguages",
	"cwtools.hover.debug",
	"cwtools.hover.scopeDisplay",
] as const;

export function isLiveSettingsChange(e: {
	affectsConfiguration(section: string): boolean;
}): boolean {
	return LIVE_SETTINGS_KEYS.some((k) => e.affectsConfiguration(k));
}

// The didChangeConfiguration payload pushed on a live settings edit: mapped
// ignore/reindex settings plus the localisation and hover settings the server
// accepts after startup.
export function buildSettingsPayload<T extends object>(
	ignoreOptions: T,
	minutes: number | undefined,
	idleSeconds: number | undefined,
	liveSettings: LiveServerSettings,
): T & {
	backgroundReindexIntervalMinutes: number;
	backgroundReindexIdleSeconds: number;
} & LiveServerSettings {
	return {
		...ignoreOptions,
		backgroundReindexIntervalMinutes:
			normalizeBackgroundReindexMinutes(minutes),
		backgroundReindexIdleSeconds:
			normalizeBackgroundReindexIdleSeconds(idleSeconds),
		...liveSettings,
	};
}
