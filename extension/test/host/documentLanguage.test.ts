import * as assert from "assert";
import * as vscode from "vscode";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { CancellationToken } from "vscode";
import sinon from "sinon";
import { EXTENSION_ID } from "../support/utils";
import type { CwtoolsApi } from "../../src/host/extension";
import type { EditorTracker } from "../../src/host/documentLanguage";

interface TrackerHarness {
	tracker: EditorTracker;
	dispose(this: void): void;
}
interface TrackerTestApi extends CwtoolsApi {
	documentLanguageTrackerForTests?: (client: object) => Promise<TrackerHarness>;
}
interface PendingRequest {
	uri: string;
	token: CancellationToken;
	settled: boolean;
	resolve(value: string[] | undefined): void;
	reject(error: Error): void;
}

function controlledClient() {
	const requests: PendingRequest[] = [];
	const client = {
		sendNotification: () => Promise.resolve(),
		sendRequest: (_type: unknown, params: { arguments: string[] }, token: CancellationToken) =>
			new Promise<string[] | undefined>((resolve, reject) => {
				const request: PendingRequest = {
					uri: params.arguments[0],
					token,
					settled: false,
					resolve: (value) => {
						if (request.settled) return;
						request.settled = true;
						cancellation.dispose();
						resolve(value);
					},
					reject: (error) => {
						if (request.settled) return;
						request.settled = true;
						cancellation.dispose();
						reject(error);
					},
				};
				const cancellation = token.onCancellationRequested(() =>
					request.reject(new vscode.CancellationError()),
				);
				requests.push(request);
			}),
	};
	return {
		client,
		requests,
		rejectPending: () => {
			for (const request of requests) {
				request.reject(new Error("test cleanup"));
			}
		},
	};
}

async function trackerWith(client: object): Promise<TrackerHarness> {
	const api = (await vscode.extensions.getExtension(EXTENSION_ID)?.activate()) as
		| TrackerTestApi
		| undefined;
	assert.ok(api, "extension API should be available");
	assert.strictEqual(
		typeof api.documentLanguageTrackerForTests,
		"function",
		"dedicated editor-tracker host label must enable the bundled tracker seam",
	);
	return api.documentLanguageTrackerForTests!(client);
}

let fixtureRoot: string;
let sandbox: sinon.SinonSandbox;
let clock: ReturnType<typeof sinon.useFakeTimers>;

async function showFixture(name: string): Promise<vscode.TextEditor> {
	const file = path.join(fixtureRoot, name);
	fs.writeFileSync(file, "portable editor classification fixture\n", "utf8");
	const uri = vscode.Uri.file(file);
	const document = await vscode.workspace.openTextDocument(uri);
	let didChange!: () => void;
	const changed = new Promise<void>((resolve) => {
		didChange = resolve;
	});
	const subscription = vscode.window.onDidChangeActiveTextEditor((editor) => {
		if (editor?.document.uri.toString() === uri.toString()) didChange();
	});
	try {
		const editor = await vscode.window.showTextDocument(document);
		if (vscode.window.activeTextEditor?.document.uri.toString() === uri.toString()) {
			didChange();
		}
		await changed;
		return editor;
	} finally {
		subscription.dispose();
	}
}

async function closeAllEditorsAndWait(): Promise<void> {
	let didClose!: () => void;
	const closed = new Promise<void>((resolve) => {
		didClose = resolve;
	});
	const subscription = vscode.window.onDidChangeActiveTextEditor((editor) => {
		if (!editor) didClose();
	});
	try {
		await vscode.commands.executeCommand("workbench.action.closeAllEditors");
		if (!vscode.window.activeTextEditor) didClose();
		await closed;
	} finally {
		subscription.dispose();
	}
}

async function settle(): Promise<void> {
	await clock.tickAsync(0);
}

suite("document language tracker", function () {
	this.timeout(30_000);

	setup(() => {
		fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-editor-tracker-"));
		sandbox = sinon.createSandbox();
		clock = sandbox.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
	});

	teardown(async () => {
		await vscode.commands.executeCommand("workbench.action.closeAllEditors");
		fs.rmSync(fixtureRoot, { recursive: true, force: true });
		sandbox.restore();
	});

	test("drops an old classification reply and applies only the latest editor", async function () {
		const transport = controlledClient();
		const commands = sandbox
			.stub(vscode.commands, "executeCommand")
			.callThrough();
		const { tracker, dispose } = await trackerWith(transport.client);
		try {
			const first = await showFixture("first.txt");
			await clock.tickAsync(200);
			assert.strictEqual(transport.requests.length, 1);
			const second = await showFixture("second.txt");
			await clock.tickAsync(200);
			commands.resetHistory();
			transport.requests[0].resolve(["stale-type"]);
			await settle();
			assert.strictEqual(transport.requests.length, 2);
			assert.strictEqual(
				commands
					.getCalls()
					.some(
						(call) =>
							call.args[0] === "setContext" && call.args[2] === true,
					),
				false,
				"the stale response must not set the graph-file context",
			);
			assert.strictEqual(transport.requests[1].uri, second.document.uri.toString());
			assert.strictEqual(transport.requests[0].uri, first.document.uri.toString());
			transport.requests[1].resolve(["current-type"]);
			await settle();
			assert.strictEqual(tracker.getLatestType(), "current-type");
		} finally {
			transport.rejectPending();
			await settle();
			dispose();
			await clock.tickAsync(2000);
		}
	});

	test("clears classification when no editor is active", async function () {
		const transport = controlledClient();
		const commands = sandbox
			.stub(vscode.commands, "executeCommand")
			.callThrough();
		const { tracker, dispose } = await trackerWith(transport.client);
		try {
			await showFixture("close-me.txt");
			await clock.tickAsync(200);
			assert.strictEqual(transport.requests.length, 1);
			transport.requests[0].resolve(["some-type"]);
			await settle();
			assert.strictEqual(tracker.getLatestType(), "some-type");
			commands.resetHistory();
			await closeAllEditorsAndWait();
			await clock.tickAsync(200);
			await tracker.classifyActiveEditor();
			assert.strictEqual(tracker.getLatestType(), "");
			assert.ok(
				commands
					.getCalls()
					.some(
						(call) =>
							call.args[0] === "setContext" &&
							call.args[1] === "cwtoolsGraphFile" &&
							call.args[2] === false,
					),
				"no active editor should clear graph-file context",
			);
		} finally {
			transport.rejectPending();
			await settle();
			dispose();
			await clock.tickAsync(2000);
		}
	});

	test("cancels a timed-out request and coalesces switches through the cooldown", async function () {
		const transport = controlledClient();
		const { tracker, dispose } = await trackerWith(transport.client);
		try {
			await showFixture("stalled.txt");
			await clock.tickAsync(200);
			assert.strictEqual(transport.requests.length, 1);
			assert.strictEqual(
				transport.requests[0].token.isCancellationRequested,
				false,
				"the request should still be active before its timeout",
			);
			await clock.tickAsync(5000);
			assert.ok(transport.requests[0].token.isCancellationRequested);
			await showFixture("superseded-during-cooldown.txt");
			await clock.tickAsync(200);
			const latest = await showFixture("latest-during-cooldown.txt");
			await clock.tickAsync(200);
			await clock.tickAsync(1599);
			assert.strictEqual(transport.requests.length, 1, "cooldown should hold the next request");
			await clock.tickAsync(1);
			assert.strictEqual(transport.requests.length, 2);
			assert.strictEqual(
				transport.requests[1].uri,
				latest.document.uri.toString(),
			);
			transport.requests[1].resolve(["latest-type"]);
			await settle();
			assert.strictEqual(tracker.getLatestType(), "latest-type");
		} finally {
			transport.rejectPending();
			await settle();
			dispose();
			await clock.tickAsync(2000);
		}
	});

	test("disposal clears pending debounce work and blocks an in-flight stale reply", async function () {
		const commands = sandbox
			.stub(vscode.commands, "executeCommand")
			.callThrough();
		let pending: TrackerHarness | undefined;
		let inFlight: TrackerHarness | undefined;
		let beforeRequest: ReturnType<typeof controlledClient> | undefined;
		let transport: ReturnType<typeof controlledClient> | undefined;
		try {
			beforeRequest = controlledClient();
			pending = await trackerWith(beforeRequest.client);
			await showFixture("debounced.txt");
			pending.dispose();
			await clock.tickAsync(250);
			assert.strictEqual(
				beforeRequest.requests.length,
				0,
				"dispose should cancel pending debounce",
			);

			transport = controlledClient();
			inFlight = await trackerWith(transport.client);
			await showFixture("in-flight.txt");
			await clock.tickAsync(200);
			assert.strictEqual(transport.requests.length, 1);
			inFlight.dispose();
			transport.requests[0].resolve(["stale-after-dispose"]);
			await settle();
			assert.strictEqual(inFlight.tracker.getLatestType(), "");
			assert.strictEqual(
				commands
					.getCalls()
					.some(
						(call) =>
							call.args[0] === "setContext" && call.args[2] === true,
					),
				false,
				"a disposed tracker must not publish stale graph-file context",
			);
		} finally {
			beforeRequest?.rejectPending();
			transport?.rejectPending();
			await settle();
			pending?.dispose();
			inFlight?.dispose();
			await clock.tickAsync(2000);
		}
	});
});
