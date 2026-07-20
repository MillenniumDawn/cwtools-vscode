import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";

// Node-only unit tests for the near-pure modules (no real vscode API needed).
// The vscode-host suites live in client/test/suite and run under
// @vscode/test-cli (see .vscode-test.mjs). Coverage here owns engine.ts and
// executable.ts; the host run excludes them so the two reports stay disjoint.
export default defineConfig({
	test: {
		include: ["client/test/unit/**/*.test.ts"],
		environment: "node",
		coverage: {
			provider: "v8",
			include: [
				"client/extension/diagnosticsSignature.ts",
				"client/extension/engine.ts",
				"client/extension/executable.ts",
				"client/extension/fileListSignature.ts",
				"client/extension/fnv1a.ts",
				"client/extension/focusTracking.ts",
				"client/extension/games.ts",
				"client/extension/logger.ts",
				"client/extension/reindexSettings.ts",
			],
			reporter: ["text-summary", "html", "lcov", "json-summary"],
			reportsDirectory: "coverage-node",
		},
	},
	resolve: {
		// engine.ts reaches vscode transitively (logger.ts). Swap in a tiny stub
		// so these tests stay out of the Electron host.
		alias: {
			vscode: fileURLToPath(
				new URL("./client/test/unit/_stubs/vscode.ts", import.meta.url),
			),
		},
	},
});
