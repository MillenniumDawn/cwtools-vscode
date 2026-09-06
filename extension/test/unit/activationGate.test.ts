import { beforeEach, expect, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";

const mocks = vi.hoisted(() => ({
	workspace: {
		workspaceFolders: undefined as { uri: string }[] | undefined,
		fs: { stat: vi.fn() },
	},
	executeCommand: vi.fn(),
	detectGameAndVanilla: vi.fn().mockResolvedValue({ languageId: "paradox" }),
	serverExe: vi.fn(),
	resolveRulesCache: vi.fn(),
	createLanguageClient: vi.fn(),
}));

vi.mock("vscode", () => ({
	workspace: mocks.workspace,
	commands: { executeCommand: mocks.executeCommand },
	Uri: { joinPath: (uri: string, name: string) => `${uri}/${name}` },
	FileType: { File: 1 },
	languages: { setLanguageConfiguration: vi.fn(() => ({ dispose() {} })) },
	window: { showErrorMessage: vi.fn() },
	l10n: { t: (message: string) => message },
}));
vi.mock("../../src/host/engine", () => ({ serverExe: mocks.serverExe }));
vi.mock("../../src/host/detectGame", () => ({
	detectGameAndVanilla: mocks.detectGameAndVanilla,
}));
vi.mock("../../src/host/rulesSetup", () => ({
	resolveRulesCache: mocks.resolveRulesCache,
	fetchRulesInBackground: vi.fn(),
}));
vi.mock("../../src/host/lspClient", () => ({
	createLanguageClient: mocks.createLanguageClient,
}));
vi.mock("../../src/host/serverNotifications", () => ({
	registerServerNotifications: vi.fn(),
}));
vi.mock("../../src/host/documentLanguage", () => ({
	registerDocumentLanguage: vi.fn(),
}));
vi.mock("../../src/host/commands", () => ({
	registerCommands: vi.fn(),
	publishCommandAvailability: vi.fn(),
}));
vi.mock("../../src/host/trustedPaths", () => ({ setTrustedRoots: vi.fn() }));
vi.mock("../../src/host/logger", () => ({
	logInfo: vi.fn(),
	logError: vi.fn(),
	errorMessage: vi.fn(),
}));
vi.mock("../../src/host/serverBlockedDialog", () => ({
	showServerBlockedDialog: vi.fn(),
}));

suite("descriptor startup gate", () => {
	beforeEach(() => {
		vi.resetModules();
		vi.clearAllMocks();
		mocks.workspace.workspaceFolders = [{ uri: "file:///project" }];
		mocks.workspace.fs.stat.mockRejectedValue(new Error("FileNotFound"));
	});

	test.each([
		"events/test.txt",
		"localisation/test.yml",
		"tests/mod/descriptor.mod",
		".metadata/metadata.json",
		"hoi4.exe",
	])("does not initialize for %s without a root descriptor", async (file) => {
		mocks.workspace.fs.stat.mockImplementation((uri: string) => {
			return uri === `file:///project/${file}`
				? Promise.resolve({ type: 1 })
				: Promise.reject(new Error("FileNotFound"));
		});
		const { activate } = await import("../../src/host/extension");
		const api = await activate({} as ExtensionContext);
		expect(mocks.workspace.fs.stat).toHaveBeenCalledExactlyOnceWith(
			"file:///project/descriptor.mod",
		);
		expect(mocks.executeCommand).toHaveBeenCalledWith(
			"setContext", "cwtoolsEnabled", false,
		);
		expect(mocks.detectGameAndVanilla).not.toHaveBeenCalled();
		expect(mocks.serverExe).not.toHaveBeenCalled();
		expect(mocks.resolveRulesCache).not.toHaveBeenCalled();
		expect(mocks.createLanguageClient).not.toHaveBeenCalled();
		expect(api.serverCommands()).toEqual([]);
		expect(api.serverOutputChannel()).toBeUndefined();
		expect(api.rulesCacheRoot()).toBeUndefined();
		expect(api.deactivate()).toBeUndefined();
	});

	test("does not initialize for a standalone file", async () => {
		mocks.workspace.workspaceFolders = undefined;
		const { activate } = await import("../../src/host/extension");
		await activate({} as ExtensionContext);
		expect(mocks.workspace.fs.stat).not.toHaveBeenCalled();
		expect(mocks.detectGameAndVanilla).not.toHaveBeenCalled();
	});

	test("rejects a directory named descriptor.mod", async () => {
		mocks.workspace.fs.stat.mockResolvedValue({ type: 2 });
		const { activate } = await import("../../src/host/extension");
		await activate({} as ExtensionContext);
		expect(mocks.detectGameAndVanilla).not.toHaveBeenCalled();
	});

	test("initializes when a workspace root contains a descriptor file", async () => {
		mocks.workspace.workspaceFolders?.push({ uri: "file:///mod" });
		mocks.workspace.fs.stat.mockImplementation((uri: string) => {
			return uri === "file:///mod/descriptor.mod"
				? Promise.resolve({ type: 1 })
				: Promise.reject(new Error("FileNotFound"));
		});
		const { activate } = await import("../../src/host/extension");
		await activate({
			globalStorageUri: { fsPath: "cache" },
			subscriptions: [],
		} as unknown as ExtensionContext);
		expect(mocks.executeCommand).toHaveBeenCalledWith(
			"setContext", "cwtoolsEnabled", true,
		);
		expect(mocks.detectGameAndVanilla).toHaveBeenCalledOnce();
		expect(mocks.serverExe).toHaveBeenCalledOnce();
	});
});
