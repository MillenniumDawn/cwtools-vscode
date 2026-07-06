// Pure helpers for the background-reindex interval setting, split out from
// lspClient.ts (which imports vscode) so the node unit tests can exercise them.

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
