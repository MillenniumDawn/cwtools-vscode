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
