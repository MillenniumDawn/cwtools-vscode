// Pure decision helpers for active-editor tracking, split out of
// documentLanguage.ts (which imports vscode) so the node unit tests can
// exercise them.

// Send a didFocusFile notification only when the focused URI changed; a repeat
// of the last-sent URI is redundant (the server already has it).
export function shouldNotifyFocus(currentUri: string, lastNotifiedUri: string | undefined): boolean {
	return currentUri !== lastNotifiedUri;
}

// How long to wait before draining a pending editor switch. After a
// getFileTypes timeout, back off so a stalled server isn't re-hit once per
// timeout window; a settled response drains immediately.
export function pendingProcessDelayMs(timedOut: boolean, backoffMs: number): number {
	return timedOut ? backoffMs : 0;
}
