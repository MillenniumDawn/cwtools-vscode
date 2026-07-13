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

// The didChangeConfiguration payload pushed on a live settings edit: the mapped
// ignore options plus the reindex interval under the server's key.
export function buildReindexSettingsPayload<T extends object>(
	ignoreOptions: T,
	minutes: number | undefined,
): T & { backgroundReindexIntervalMinutes: number } {
	return {
		...ignoreOptions,
		backgroundReindexIntervalMinutes: normalizeBackgroundReindexMinutes(minutes),
	};
}
