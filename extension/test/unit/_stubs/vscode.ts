// Minimal `vscode` stub for the node unit tests (vitest aliases the bare
// `vscode` import to this). Extend it as more modules come under node coverage.
// A test that needs a spy or a different shape spreads this module in its
// `vi.mock("vscode", ...)` factory and overrides only that member.

import { fileURLToPath, pathToFileURL } from "node:url";

export const window = {
	createOutputChannel(_name: string, _options?: { log: true }) {
		return {
			appendLine: (_message: string) => {},
			info: (_message: string) => {},
			warn: (_message: string) => {},
			error: (_message: string) => {},
			dispose: () => {},
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

interface StubUri {
	readonly scheme: string;
	readonly fsPath: string;
	toString(): string;
}

// Only fsPath is enumerable, so a test can deepStrictEqual against { fsPath }.
function stubUri(scheme: string, fsPath: string, text: string): StubUri {
	return Object.defineProperties(
		{ fsPath },
		{ scheme: { value: scheme }, toString: { value: () => text } },
	) as StubUri;
}

export const Uri = {
	file: (fsPath: string) =>
		stubUri("file", fsPath, pathToFileURL(fsPath).toString()),
	parse: (value: string) => {
		const url = new URL(value);
		const scheme = url.protocol.slice(0, -1);
		return stubUri(
			scheme,
			scheme === "file" ? fileURLToPath(url) : url.pathname,
			value,
		);
	},
};

export class CancellationError extends Error {}

export const ProgressLocation = { SourceControl: 1, Window: 10, Notification: 15 };

export class RelativePattern {
	constructor(
		readonly baseUri: unknown,
		readonly pattern: string,
	) {}
}

export class EventEmitter {
	readonly event = () => ({ dispose: () => {} });
	fire(): void {}
	dispose(): void {}
}

export const TreeItemCollapsibleState = { None: 0, Collapsed: 1, Expanded: 2 };

export class TreeItem {
	command?: unknown;
	contextValue?: string;
	resourceUri?: unknown;
	constructor(
		public label: string,
		public collapsibleState: number,
	) {}
}

export const workspace = {
	getConfiguration: (_section?: string) => ({
		get: (_key: string, defaultValue?: unknown) => defaultValue,
	}),
};
