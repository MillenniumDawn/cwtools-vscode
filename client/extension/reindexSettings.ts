// Pure helpers for the background-reindex and ignore settings, split out from
// lspClient.ts (which imports vscode) so the node unit tests can exercise them.

// The server reads remapped keys (ignoreFilePatterns/ignoredErrorCodes); the
// `cwtools.*` settings use different names. Map them here so both the initial
// initializationOptions and the live didChangeConfiguration payload agree.
// ignore_patterns are already globs; errors.ignorefiles lists bare file names,
// so turn each into a **/<name> glob to match anywhere.
export function mapIgnoreOptions(
	ignorePatterns: readonly string[] | undefined,
	ignoreFiles: readonly string[] | undefined,
	ignoredCodes: readonly string[] | undefined,
): { ignoreFilePatterns: string[]; ignoredErrorCodes: string[] } {
	return {
		ignoreFilePatterns: [
			...(ignorePatterns ?? []),
			...(ignoreFiles ?? []).map(f => (f.includes('/') ? f : `**/${f}`)),
		],
		ignoredErrorCodes: [...(ignoredCodes ?? [])],
	};
}

// Minutes between the server's periodic background re-index passes. An unset
// setting (undefined) falls back to 30; an explicit 0 disables the loop and is
// preserved, as are the user's negative/fractional values (the server clamps).
export function normalizeBackgroundReindexMinutes(value: number | undefined): number {
	return value ?? 30;
}

// Seconds of user inactivity before a background pass is allowed to start, so
// a rescan never competes with a request the user is waiting on. Unset falls
// back to the server's own default of 15.
export function normalizeBackgroundReindexIdleSeconds(value: number | undefined): number {
	return value ?? 15;
}

// The didChangeConfiguration payload pushed on a live settings edit: the mapped
// ignore options plus the reindex interval and idle window under the server's
// keys.
export function buildReindexSettingsPayload<T extends object>(
	ignoreOptions: T,
	minutes: number | undefined,
	idleSeconds: number | undefined,
): T & { backgroundReindexIntervalMinutes: number; backgroundReindexIdleSeconds: number } {
	return {
		...ignoreOptions,
		backgroundReindexIntervalMinutes: normalizeBackgroundReindexMinutes(minutes),
		backgroundReindexIdleSeconds: normalizeBackgroundReindexIdleSeconds(idleSeconds),
	};
}
