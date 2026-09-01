import * as assert from "assert";
import { activate } from "../support/utils";

// Last file in the labels that run it: it stops the shared language client, so
// any suite after it would find no server.
suite("deactivate", function () {
	this.timeout(60 * 1000);

	// VS Code exposes no way to deactivate a single extension, so the activation
	// API hands back the module's own deactivate(). That is the same function the
	// host calls when the window closes, not a second copy of it.
	test("stops the running client and tolerates a second call", async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		assert.ok(
			api.serverCommands().length > 0,
			"the client should be running before deactivate",
		);

		await api.deactivate();

		// stop() clears initializeResult once the server has answered shutdown and
		// exit, so an empty command list is the handshake having completed.
		assert.deepStrictEqual(
			api.serverCommands(),
			[],
			"deactivate should have stopped the client",
		);

		// The client is disposed from context.subscriptions right after deactivate()
		// resolves, which stops it a second time.
		await api.deactivate();
	});
});
