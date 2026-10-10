import { languages, Uri } from "vscode";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import type { TextDocument } from "vscode";
import type * as VscodeStub from "./_stubs/vscode";
import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";

const {
	activeEditor,
	disposable,
	executeCommand,
	logError,
	logInfo,
	onDidChangeActiveTextEditor,
	documentState,
	onDidOpenTextDocument,
	uriParse,
} = vi.hoisted(() => ({
	uriParse: vi.fn<(value: string) => { scheme: string; fsPath: string }>(),
	documentState: { documents: [] as TextDocument[] },
	onDidOpenTextDocument: vi.fn((_listener: unknown) => ({ dispose() {} })),
	activeEditor: {
		document: {
			languageId: "paradox",
			uri: {
				toString: () => "file:///workspace/events/focus.txt",
			},
		},
	},
	disposable: { dispose: () => {} },
	executeCommand: vi.fn().mockResolvedValue(undefined),
	logError: vi.fn(),
	logInfo: vi.fn(),
	onDidChangeActiveTextEditor: vi.fn((_listener: unknown) => disposable),
}));

vi.mock("vscode", async (importOriginal) => {
	const original = await importOriginal<typeof VscodeStub>();
	return {
		...original,
		Uri: {
			...original.Uri,
			parse: uriParse.mockImplementation((value: string) => {
				if (/^file:\/\/(?:[?#]|$)/.test(value)) return { scheme: "file", fsPath: path.sep };
				if (value.startsWith("file:") && !value.startsWith("file://")) {
					// VS Code's _referenceResolution prefixes an empty or relative
					// file path with '/', even when Uri.parse's strict flag is set.
					const suffix = value.slice("file:".length).replace(/^\//, "");
					return { scheme: "file", fsPath: path.sep + suffix.replace(/\//g, path.sep) };
				}
				return { scheme: new URL(value).protocol.slice(0, -1), fsPath: fileURLToPath(value) };
			}),
		},
		StatusBarAlignment: { Left: 1 },
		l10n: { t: (message: string) => message },
		CancellationTokenSource: class {
			private readonly cancellationListeners: Array<() => void> = [];
			token = {
				isCancellationRequested: false,
				onCancellationRequested: (listener: () => void) => {
					this.cancellationListeners.push(listener);
					return { dispose: () => {} };
				},
			};

			cancel(): void {
				this.token.isCancellationRequested = true;
				for (const listener of this.cancellationListeners) listener();
			}

			dispose(): void {}
		},
		commands: {
			executeCommand,
			registerCommand: vi.fn(() => disposable),
		},
		languages: {
			setTextDocumentLanguage: vi.fn().mockResolvedValue(undefined),
		},
		window: {
			createStatusBarItem: () => ({ text: "", show() {}, dispose() {} }),
			activeTextEditor: activeEditor,
			onDidChangeActiveTextEditor,
		},
		workspace: {
			get textDocuments() { return documentState.documents; },
			onDidOpenTextDocument,
		},
	};
});

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: {} },
	State: { Stopped: 1, Running: 2, Starting: 3, StartFailed: 4 },
}));

vi.mock("../../src/host/fileExplorer", () => ({
	FileExplorer: class { dispose() {} refresh() {} },
}));

vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger({ logError, logInfo }),
);

import { registerDocumentLanguage } from "../../src/host/documentLanguage";
import { gameContentRoots } from "../../src/host/gameContentRoots";
import { registerServerNotifications } from "../../src/host/serverNotifications";
import { State } from "vscode-languageclient/node";

const contentRoot = path.resolve("document-language-workspace");

suite("documentLanguage", () => {
	beforeEach(() => {
		documentState.documents = [];
	});

	test("logs a rejected focus notification instead of propagating it", async () => {
		const failure = new Error("Client is not running");
		const sendNotification = vi.fn().mockRejectedValue(failure);
		const sendRequest = vi.fn();
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
			() => [contentRoot],
		);

		await assert.doesNotReject(() => tracker.classifyActiveEditor());
		assert.deepStrictEqual(sendNotification.mock.calls, [
			["didFocusFile", { uri: "file:///workspace/events/focus.txt" }],
		]);
		assert.strictEqual(sendRequest.mock.calls.length, 0);
		assert.deepStrictEqual(logError.mock.calls, [
			["didChangeActiveTextEditor failed", failure],
		]);
	});

	test("clears the latest type when getFileTypes returns no type", async () => {
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi
			.fn()
			.mockResolvedValueOnce(["idea"])
			.mockResolvedValueOnce([]);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
			() => [contentRoot],
		);

		await tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "idea");
		await tracker.classifyActiveEditor();

		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphFile", true],
			["setContext", "cwtoolsGraphFile", false],
		]);
	});

	test("clears the latest type when getFileTypes fails", async () => {
		const failure = new Error("server down");
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi
			.fn()
			.mockResolvedValueOnce(["idea"])
			.mockRejectedValueOnce(failure);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
			() => [contentRoot],
		);

		await tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "idea");
		await tracker.classifyActiveEditor();

		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphFile", true],
			["setContext", "cwtoolsGraphFile", false],
		]);
		assert.deepStrictEqual(logError.mock.calls, [
			["didChangeActiveTextEditor getFileTypes failed", failure],
		]);
	});

	test("clears the latest type during the active editor debounce", async () => {
		vi.useFakeTimers();
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi.fn().mockResolvedValue(["idea"]);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
			() => [contentRoot],
		);
		await tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "idea");

		const listener = onDidChangeActiveTextEditor.mock.calls[0]?.[0] as
			| ((editor: typeof activeEditor) => void)
			| undefined;
		assert.ok(listener);
		listener(activeEditor);

		assert.strictEqual(tracker.getLatestType(), "");
		await vi.advanceTimersByTimeAsync(200);
	});

	test("clears the cached type before coalescing an in-flight request", async () => {
		const requestResolvers: Array<(data: string[]) => void> = [];
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi
			.fn()
			.mockResolvedValueOnce(["idea"])
			.mockImplementation(
				() =>
					new Promise<string[]>((resolve) => {
						requestResolvers.push(resolve);
					}),
			);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
			() => [contentRoot],
		);

		await tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "idea");
		const first = tracker.classifyActiveEditor();
		await vi.waitFor(() => assert.strictEqual(requestResolvers.length, 1));
		assert.strictEqual(tracker.getLatestType(), "");

		const second = tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "");
		requestResolvers.shift()!([]);
		await vi.waitFor(() => assert.strictEqual(requestResolvers.length, 1));
		requestResolvers.shift()!([]);
		await Promise.all([first, second]);
	});

	test("cancels and backs off a timed-out getFileTypes request", async () => {
		vi.useFakeTimers();
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi.fn(
			(
				_type: unknown,
				_params: unknown,
				token: {
					onCancellationRequested: (listener: () => void) => unknown;
				},
			) =>
				new Promise<string[]>((_resolve, reject) => {
					token.onCancellationRequested(() => reject(new Error("cancelled")));
				}),
		);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
			() => [contentRoot],
		);

		const classification = tracker.classifyActiveEditor();
		await vi.advanceTimersByTimeAsync(5000);
		await vi.advanceTimersByTimeAsync(2000);
		await classification;

		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphFile", false],
		]);
		assert.deepStrictEqual(logInfo.mock.calls, [
			["didChangeActiveTextEditor getFileTypes timed out after 5000ms"],
		]);
		assert.deepStrictEqual(logError.mock.calls, []);
	});

	test("contains a rejected notification from an editor change", async () => {
		vi.useFakeTimers();
		const failure = new Error("Client is not running");
		const sendNotification = vi.fn().mockRejectedValue(failure);
		await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest: vi.fn() } as unknown as LanguageClient,
			"paradox",
			() => [contentRoot],
		);
		const listener = onDidChangeActiveTextEditor.mock.calls[0]?.[0] as
			| ((editor: typeof activeEditor) => void)
			| undefined;
		assert.ok(listener);

		listener(activeEditor);
		await vi.advanceTimersByTimeAsync(200);

		assert.deepStrictEqual(sendNotification.mock.calls, [
			["didFocusFile", { uri: "file:///workspace/events/focus.txt" }],
		]);
		assert.deepStrictEqual(logError.mock.calls, [
			["didChangeActiveTextEditor failed", failure],
		]);
	});

	suite("plaintext promotion", () => {
		const makeDocument = (file: string, languageId = "plaintext", scheme = "file") => ({
			languageId,
			uri: { fsPath: file, scheme, toString: () => `file://${file}` },
		}) as TextDocument;
		const roots = [contentRoot, path.resolve("parent-mod"), path.resolve("vanilla")];
		const valid = [
			makeDocument(path.join(contentRoot, "common", "ideas", "idea.txt")),
			makeDocument(path.join(contentRoot, "interface", "test.gfx")),
			makeDocument(path.join(roots[1], "events", "parent.txt")),
			makeDocument(path.join(roots[2], "game", "common", "ideas", "base.txt")),
		];
		const unrelated = [
			makeDocument(path.join(path.resolve("unrelated"), "common", "notes.txt")),
			makeDocument(path.join(path.resolve("outside"), "image.gfx")),
			makeDocument(path.join(`${contentRoot}-other`, "events", "notes.txt")),
			makeDocument(path.join(contentRoot, "..", "outside", "common", "notes.txt")),
			makeDocument(path.join(contentRoot, "notes.txt")),
			makeDocument(path.join(contentRoot, "common", "notes.txt"), "markdown"),
			makeDocument(path.join(contentRoot, "common", "notes.txt"), "plaintext", "untitled"),
		];
		const register = () => registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification: vi.fn(), sendRequest: vi.fn() } as unknown as LanguageClient,
			"paradox", () => roots,
		);

		test("promotes auto-discovered vanilla with the cache setting unset and keeps unrelated roots plaintext", async () => {
			for (const [value, suffix] of [["file:", ""], ["file:relative", "relative"], ["file:/relative", "relative"], ["file://", ""], ["file://?q=/path", ""], ["file://#x/path", ""]]) {
				assert.strictEqual(Uri.parse(value, true).fsPath, path.sep + suffix);
			}
			uriParse.mockClear();
			const vanilla = path.resolve("auto-discovered-vanilla");
			const physical = path.resolve("physical-vanilla");
			const vanillaRoots = [vanilla, physical].map((root) => Uri.file(root).toString());
			const alreadyOpen = makeDocument(path.join(vanilla, "common", "ideas", "base.txt"));
			const definition = makeDocument(path.join(physical, "common", "ideas", "base.txt"));
			const external = makeDocument(path.join(`${vanilla}-other`, "common", "notes.txt"));
			const physicalExternal = makeDocument(path.join(`${physical}-other`, "common", "notes.txt"));
			documentState.documents = [alreadyOpen, definition, external, physicalExternal, ...unrelated];
			const handlers = new Map<string, (params: unknown) => void>();
			let stateChanged: (event: { oldState: number; newState: number }) => void = () => {};
			const client = {
				sendNotification: vi.fn(), sendRequest: vi.fn(),
				onNotification: (method: string, handler: (params: unknown) => void) => {
					handlers.set(method, handler);
					return disposable;
				},
				onDidChangeState: (handler: typeof stateChanged) => { stateChanged = handler; return disposable; },
			} as unknown as LanguageClient;
			const context = { subscriptions: [] } as unknown as ExtensionContext;
			const tracker = await registerDocumentLanguage(
				context, client,
				"paradox", () => gameContentRoots(contentRoot),
			);
			registerServerNotifications(context, client, tracker.updateVanillaRoots);
			const updateFileList = handlers.get("updateFileList");
			assert.ok(updateFileList);
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, []);

			updateFileList({ fileList: [], vanillaRoots });
			await Promise.resolve();
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, [[alreadyOpen, "paradox"], [definition, "paradox"]]);
			vi.mocked(languages.setTextDocumentLanguage).mockClear();
			const onOpen = onDidOpenTextDocument.mock.calls[0][0] as (doc: TextDocument) => Promise<void>;
			const newlyOpen = makeDocument(path.join(vanilla, "events", "base.txt"));
			const newlyDefined = makeDocument(path.join(physical, "events", "base.txt"));
			await onOpen(newlyOpen);
			await onOpen(newlyDefined);
			await onOpen(external);
			await onOpen(physicalExternal);
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, [[newlyOpen, "paradox"], [newlyDefined, "paradox"]]);

			stateChanged({ oldState: State.Running, newState: State.Starting });
			// A late notification from the stopped server cannot restore its root.
			updateFileList({ fileList: [], vanillaRoots });
			vi.mocked(languages.setTextDocumentLanguage).mockClear();
			await onOpen(newlyOpen);
			await onOpen(newlyDefined);
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, []);
			stateChanged({ oldState: State.Starting, newState: State.Running });
			for (const invalid of [42, {}, "relative/install", [42, {}, "relative/install", "https://example.com/install", "file:", "file:relative", "file:/relative", "file://", "file://?q=/path", "file://#x/path"]]) {
				updateFileList({ fileList: [], vanillaRoots: invalid });
				await onOpen(newlyOpen);
				await onOpen(newlyDefined);
			}
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, []);
			assert.ok(uriParse.mock.calls.every(([root]) => /^file:\/\/[^/?#]*\//.test(root)), "malformed file URIs must be rejected before VS Code can normalize them");
			updateFileList({ fileList: [], vanillaRoots });
			await onOpen(newlyOpen);
			assert.ok(vi.mocked(languages.setTextDocumentLanguage).mock.calls.some(([doc]) => doc === newlyOpen));
		});

		test("refreshes configured roots after a server restart", async () => {
			const previous = path.resolve("previous-vanilla");
			const replacement = path.resolve("replacement-vanilla");
			let configured = previous;
			const tracker = await registerDocumentLanguage(
				{ subscriptions: [] } as unknown as ExtensionContext,
				{ sendNotification: vi.fn(), sendRequest: vi.fn() } as unknown as LanguageClient,
				"paradox", () => gameContentRoots(contentRoot, [], configured),
			);
			configured = replacement;
			await tracker.updateVanillaRoots([]);
			const onOpen = onDidOpenTextDocument.mock.calls[0][0] as (doc: TextDocument) => Promise<void>;
			await onOpen(makeDocument(path.join(previous, "common", "notes.txt")));
			const current = makeDocument(path.join(replacement, "common", "base.txt"));
			await onOpen(current);
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, [[current, "paradox"]]);
		});

		test("skips the recheck when a roots callback returns equal roots in a new array", async () => {
			const tracker = await registerDocumentLanguage(
				{ subscriptions: [] } as unknown as ExtensionContext,
				{ sendNotification: vi.fn(), sendRequest: vi.fn() } as unknown as LanguageClient,
				"paradox", () => gameContentRoots(contentRoot),
			);
			documentState.documents = [makeDocument(path.join(contentRoot, "common", "ideas", "idea.txt"))];
			await tracker.updateVanillaRoots([]);
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, []);
		});

		test("promotes only recognized content among documents already open", async () => {
			documentState.documents = [...valid, ...unrelated];
			await register();
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, valid.map((doc) => [doc, "paradox"]));
		});

		test("does not treat the content root's ancestor names as directory hints", async () => {
			const root = path.join(contentRoot, "common", "opaque-mod");
			documentState.documents = [makeDocument(path.join(root, "notes.txt"))];
			await registerDocumentLanguage(
				{ subscriptions: [] } as unknown as ExtensionContext,
				{ sendNotification: vi.fn(), sendRequest: vi.fn() } as unknown as LanguageClient,
				"paradox", () => [root],
			);
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, []);
		});

		test("applies the same containment when new documents open", async () => {
			await register();
			const listener = onDidOpenTextDocument.mock.calls[0][0] as (doc: TextDocument) => Promise<void>;
			for (const doc of [...valid, ...unrelated]) await listener(doc);
			assert.deepStrictEqual(vi.mocked(languages.setTextDocumentLanguage).mock.calls, valid.map((doc) => [doc, "paradox"]));
		});
	});

	suite("stale editor classification", () => {
		interface Editor {
			document: { languageId: string; uri: { toString(): string; scheme: string; fsPath: string } };
		}
		const editorFor = (name: string): Editor => ({
			document: {
				languageId: "paradox",
				uri: { toString: () => `file:///workspace/events/${name}.txt`, scheme: "file", fsPath: path.join(contentRoot, "events", `${name}.txt`) },
			},
		});
		const editorA = editorFor("a");
		const editorB = editorFor("b");
		const flush = () => vi.advanceTimersByTimeAsync(0);
		const graphFileValues = (): unknown[] => {
			const calls: unknown[][] = executeCommand.mock.calls;
			return calls
				.filter(([id, key]) => id === "setContext" && key === "cwtoolsGraphFile")
				.map((call) => call[2]);
		};
		const lastGraphFile = (): unknown => {
			const values = graphFileValues();
			return values[values.length - 1];
		};

		async function register() {
			const subscriptions: Array<{ dispose(): void }> = [];
			const requests: Array<{
				uri: string;
				resolve: (types: string[]) => void;
			}> = [];
			const sendNotification = vi.fn().mockResolvedValue(undefined);
			const sendRequest = vi.fn(
				(_type: unknown, params: { arguments: string[] }) =>
					new Promise<string[]>((resolve) => {
						requests.push({ uri: params.arguments[0], resolve });
					}),
			);
			const tracker = await registerDocumentLanguage(
				{ subscriptions } as unknown as ExtensionContext,
				{ sendNotification, sendRequest } as unknown as LanguageClient,
				"paradox",
				() => [contentRoot],
			);
			const listener = onDidChangeActiveTextEditor.mock.calls[0]?.[0] as
				| ((editor: Editor | undefined) => void)
				| undefined;
			assert.ok(listener);
			return { tracker, subscriptions, requests, sendNotification, listener };
		}

		beforeEach(() => {
			vi.useFakeTimers();
		});

		for (const dispose of [false, true]) {
			test(`drops a pending language upgrade after ${dispose ? "disposal" : "focus changes"}`, async () => {
				const { subscriptions, requests, sendNotification, listener } = await register();
				let finish!: () => void;
				const plaintext = { document: { ...editorA.document, languageId: "plaintext" } };
				vi.mocked(languages.setTextDocumentLanguage).mockImplementationOnce(() => new Promise((resolve) => { finish = () => { plaintext.document.languageId = "paradox"; resolve(undefined as never); }; }));
				listener(plaintext);
				await vi.advanceTimersByTimeAsync(200);
				if (dispose) {
					for (const subscription of subscriptions) subscription.dispose();
				} else {
					listener(editorB);
					await vi.advanceTimersByTimeAsync(200);
					requests[0].resolve(["focus"]);
					await flush();
				}
				finish();
				await flush();
				assert.deepStrictEqual(sendNotification.mock.calls, dispose ? [] : [["didFocusFile", { uri: editorB.document.uri.toString() }]]);
				assert.strictEqual(requests.length, dispose ? 0 : 1);
			});
		}

		test("applies the reply for the only focused editor", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "focus");
			assert.strictEqual(lastGraphFile(), true);
		});

		test("drops a late reply after the active editor goes away", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			assert.strictEqual(requests.length, 1);
			listener(undefined);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "");
			assert.strictEqual(lastGraphFile(), false);
		});

		test("drops a late reply for the previous editor and applies the new one", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			listener(editorB);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "");
			assert.strictEqual(lastGraphFile(), false);
			assert.deepStrictEqual(
				requests.map((request) => request.uri),
				["file:///workspace/events/a.txt", "file:///workspace/events/b.txt"],
			);

			requests[1].resolve(["idea"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "idea");
			assert.strictEqual(lastGraphFile(), true);
		});

		test("never classifies a queued editor once nothing is focused", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			listener(editorB);
			await vi.advanceTimersByTimeAsync(200);
			listener(undefined);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(requests.length, 1);
			assert.strictEqual(tracker.getLatestType(), "");
			assert.strictEqual(lastGraphFile(), false);
		});

		test("writes nothing after teardown", async () => {
			const { tracker, subscriptions, requests, sendNotification, listener } =
				await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			listener(editorB);
			for (const subscription of subscriptions) subscription.dispose();
			requests[0].resolve(["focus"]);
			await vi.advanceTimersByTimeAsync(1000);

			assert.strictEqual(tracker.getLatestType(), "");
			assert.deepStrictEqual(sendNotification.mock.calls, [
				["didFocusFile", { uri: "file:///workspace/events/a.txt" }],
			]);
			assert.strictEqual(requests.length, 1);
			assert.ok(!graphFileValues().includes(true));
			assert.strictEqual(vi.getTimerCount(), 0);
		});
	});
});
