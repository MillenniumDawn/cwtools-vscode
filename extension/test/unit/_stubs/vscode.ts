// Minimal `vscode` stub for the node unit tests (vitest aliases the bare
// `vscode` import to this). It only covers the runtime surface the pure
// modules touch at import time — currently logger.ts's output channel. Extend
// it as more modules come under node coverage.

export const window = {
	createOutputChannel(_name: string) {
		return {
			appendLine: (_message: string) => {},
		};
	},
};

// Matches what the real l10n.t does with no bundle loaded: hand back the
// English message with its {n} placeholders filled in, so a test can assert the
// string a user would see.
export const l10n = {
	t(message: string, ...args: Array<string | number | boolean>): string {
		return message.replace(/\{(\d+)\}/g, (placeholder, index: string) => {
			const arg = args[Number(index)];
			return arg === undefined ? placeholder : String(arg);
		});
	},
};
