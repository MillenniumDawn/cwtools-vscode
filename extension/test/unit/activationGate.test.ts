import { beforeEach, expect, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";
import * as path from "node:path";
import * as os from "node:os";
import type * as NodeFs from "fs/promises";
import type * as Engine from "../../src/host/engine";
import type * as Detection from "../../src/host/detectGame";

const mocks = vi.hoisted(() => ({
	workspace: {
		workspaceFolders: undefined as { uri: unknown }[] | undefined,
		fs: { stat: vi.fn() },
		findFiles: vi.fn().mockResolvedValue([]),
		getConfiguration: vi.fn(() => ({
			get: vi.fn((_key: string, defaultValue?: unknown) => defaultValue),
		})),
	},
	executeCommand: vi.fn(),
	createOutputChannel: vi.fn(() => ({
		appendLine: vi.fn(),
		info: vi.fn(),
		warn: vi.fn(),
		error: vi.fn(),
		show: vi.fn(),
		dispose: vi.fn(),
	})),
	detectGameAndVanilla: vi.fn().mockResolvedValue({ languageId: "paradox" }),
	initializeLogger: vi.fn(),
	serverExe: vi.fn(),
	resolveRulesCache: vi.fn(),
	createLanguageClient: vi.fn(),
	registerCommands: vi.fn(),
	fsStat: vi.fn(),
	fsChmod: vi.fn(),
	client: {
		initializeResult: { capabilities: {} },
		outputChannel: { appendLine: vi.fn() },
		registerProposedFeatures: vi.fn(),
		start: vi.fn(),
		dispose: vi.fn(),
	},
	tracker: { classifyActiveEditor: vi.fn() },
	notifications: {
		initialScanDone: Promise.resolve(),
		statusText: vi.fn(),
		markStopped: vi.fn(),
	},
}));

vi.mock("fs/promises", async (importOriginal) => ({
	...await importOriginal<typeof NodeFs>(),
	stat: mocks.fsStat,
	chmod: mocks.fsChmod,
}));
vi.mock("vscode", () => ({
	workspace: mocks.workspace,
	RelativePattern: class RelativePattern {
		constructor(public readonly root: unknown, public readonly pattern: string) {}
	},
	commands: { executeCommand: mocks.executeCommand },
	Uri: { joinPath: (uri: string, name: string) => `${uri}/${name}` },
	FileType: { File: 1, Directory: 2 },
	languages: { setLanguageConfiguration: vi.fn(() => ({ dispose() {} })) },
	window: {
		showErrorMessage: vi.fn(),
		createOutputChannel: mocks.createOutputChannel,
	},
	l10n: { t: (message: string) => message },
}));
vi.mock("../../src/host/engine", async (importOriginal) => ({
	...await importOriginal<typeof Engine>(),
	serverExe: mocks.serverExe,
}));
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
	registerServerNotifications: vi.fn(() => mocks.notifications),
}));
vi.mock("../../src/host/documentLanguage", () => ({
	registerDocumentLanguage: vi.fn(() => mocks.tracker),
}));
vi.mock("../../src/host/commands", () => ({
	registerCommands: mocks.registerCommands,
}));
vi.mock("../../src/host/trustedPaths", () => ({ setTrustedRoots: vi.fn() }));
vi.mock("../../src/host/logger", () => ({
	initializeLogger: mocks.initializeLogger,
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
		mocks.detectGameAndVanilla.mockResolvedValue({ languageId: "paradox" });
		mocks.workspace.getConfiguration.mockImplementation(() => ({
			get: vi.fn((_key: string, defaultValue?: unknown) => defaultValue),
		}));
		mocks.workspace.workspaceFolders = [{ uri: "file:///project" }];
		mocks.workspace.fs.stat.mockRejectedValue(new Error("FileNotFound"));
		mocks.fsStat.mockResolvedValue({ mode: 0o755 });
		mocks.fsChmod.mockResolvedValue(undefined);
		mocks.serverExe.mockReturnValue("/bin/cwtools-server");
		mocks.resolveRulesCache.mockResolvedValue({
			rulesCache: "/rules",
			fetchUpstream: false,
		});
		mocks.createLanguageClient.mockReturnValue(mocks.client);
		mocks.client.start.mockResolvedValue(undefined);
		mocks.createOutputChannel.mockClear();
	});

	test("does not initialize when disabled", async () => {
		const get = vi.fn().mockReturnValue(false);
		mocks.workspace.getConfiguration.mockReturnValue({ get });
		const { activate } = await import("../../src/host/extension");
		const api = await activate({} as ExtensionContext);
		expect(get).toHaveBeenCalledWith("enable", true);
		expect(mocks.workspace.fs.stat).not.toHaveBeenCalled();
		expect(mocks.executeCommand).not.toHaveBeenCalled();
		expect(mocks.detectGameAndVanilla).not.toHaveBeenCalled();
		expect(mocks.createOutputChannel).not.toHaveBeenCalled();
		expect(mocks.serverExe).not.toHaveBeenCalled();
		expect(mocks.resolveRulesCache).not.toHaveBeenCalled();
		expect(mocks.createLanguageClient).not.toHaveBeenCalled();
		expect(mocks.registerCommands).not.toHaveBeenCalled();
		expect(api.serverCommands()).toEqual([]);
		expect(api.serverOutputChannel()).toBeUndefined();
		expect(api.rulesCacheRoot()).toBeUndefined();
		expect(api.deactivate()).toBeUndefined();
	});

	test.each([
		"events/test.txt",
		"localisation/test.yml",
		"tests/mod/descriptor.mod",
		".metadata/metadata.json",
		"hoi4.exe",
	])("does not initialize for %s without a root descriptor", async (file: string) => {
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
			"setContext",
			"cwtoolsEnabled",
			false,
		);
		expect(mocks.detectGameAndVanilla).not.toHaveBeenCalled();
		expect(mocks.createOutputChannel).not.toHaveBeenCalled();
		expect(mocks.serverExe).not.toHaveBeenCalled();
		expect(mocks.resolveRulesCache).not.toHaveBeenCalled();
		expect(mocks.createLanguageClient).not.toHaveBeenCalled();
		expect(mocks.registerCommands).not.toHaveBeenCalled();
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
		expect(mocks.executeCommand).toHaveBeenCalledWith(
			"setContext",
			"cwtoolsEnabled",
			false,
		);
		expect(mocks.detectGameAndVanilla).not.toHaveBeenCalled();
		expect(mocks.createOutputChannel).not.toHaveBeenCalled();
		expect(mocks.registerCommands).not.toHaveBeenCalled();
	});

	test("rejects a directory named descriptor.mod", async () => {
		mocks.workspace.fs.stat.mockResolvedValue({ type: 2 });
		const { activate } = await import("../../src/host/extension");
		await activate({} as ExtensionContext);
		expect(mocks.executeCommand).toHaveBeenCalledWith(
			"setContext",
			"cwtoolsEnabled",
			false,
		);
		expect(mocks.detectGameAndVanilla).not.toHaveBeenCalled();
		expect(mocks.createOutputChannel).not.toHaveBeenCalled();
		expect(mocks.registerCommands).not.toHaveBeenCalled();
	});

	test("initializes the first descriptor root and starts its client", async () => {
		const unrelated = { uri: "file:///project" };
		const mod = { uri: { fsPath: "/mod", toString: () => "file:///mod" } };
		mocks.workspace.workspaceFolders = [unrelated, mod];
		mocks.workspace.fs.stat.mockImplementation((uri: string) => {
			return uri === "file:///mod/descriptor.mod"
				? Promise.resolve({ type: 1 })
				: Promise.reject(new Error("FileNotFound"));
		});
		const { activate } = await import("../../src/host/extension");
		const context = {
			globalStorageUri: { fsPath: "cache" },
			subscriptions: [],
		} as unknown as ExtensionContext;
		await activate(context);
		expect(mocks.executeCommand).toHaveBeenCalledWith(
			"setContext",
			"cwtoolsEnabled",
			true,
		);
		expect(mocks.detectGameAndVanilla).toHaveBeenCalledOnce();
		expect(mocks.detectGameAndVanilla).toHaveBeenCalledWith(mod);
		expect(mocks.resolveRulesCache).toHaveBeenCalledWith(
			"paradox",
			expect.any(String),
			"/mod",
		);
		expect(mocks.createOutputChannel).toHaveBeenCalledOnce();
		expect(mocks.initializeLogger).toHaveBeenCalledWith(
			mocks.createOutputChannel.mock.results[0]?.value,
		);
		expect(context.subscriptions).toContain(
			mocks.createOutputChannel.mock.results[0]?.value,
		);
		expect(mocks.serverExe).toHaveBeenCalledWith(context);
		expect(mocks.createLanguageClient).toHaveBeenCalledWith(
			context,
			expect.objectContaining({
				workspaceFolder: mod,
				serverExe: "/bin/cwtools-server",
			}),
			expect.any(Function),
		);
		expect(mocks.client.registerProposedFeatures).toHaveBeenCalledOnce();
		expect(mocks.registerCommands).toHaveBeenCalledWith(
			context,
			mocks.client,
			mocks.tracker,
			"/bin/cwtools-server",
		);
		expect(context.subscriptions).toContain(mocks.client);
		expect(mocks.client.start).toHaveBeenCalledOnce();
	});
	test("initializes HOI4 from real content under a misleading ancestor without remote rules", async () => {
		const fs = await import("node:fs/promises");
		const temp = await fs.mkdtemp(path.join(os.tmpdir(), "cwtools-init-"));
		try {
			const root = path.join(temp, "stellaris", "Millennium-Dawn");
			await fs.mkdir(path.join(root, "common", "ai_strategy"), { recursive: true });
			await fs.writeFile(path.join(root, "descriptor.mod"), 'name="MD fixture"');
			const mod = { uri: { fsPath: root, toString: () => `file://${root}` }, name: "Millennium-Dawn", index: 0 };
			mocks.workspace.workspaceFolders = [mod];
			mocks.workspace.fs.stat.mockResolvedValue({ type: 1 });
			const { detectGameAndVanilla } = await vi.importActual<typeof Detection>("../../src/host/detectGame");
			mocks.detectGameAndVanilla.mockImplementation(detectGameAndVanilla);
			const { activate } = await import("../../src/host/extension");
			await activate({ globalStorageUri: { fsPath: "cache" }, subscriptions: [] } as unknown as ExtensionContext);
			expect(mocks.resolveRulesCache).toHaveBeenCalledWith("hoi4", expect.any(String), root);
			expect(mocks.createLanguageClient).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({
				language: "hoi4", workspaceFolder: mod,
			}), expect.any(Function));
		} finally {
			await fs.rm(temp, { recursive: true, force: true });
		}
	});

});
