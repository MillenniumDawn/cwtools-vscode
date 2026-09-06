import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";

// Node-only unit tests for the near-pure modules (no real vscode API needed).
// The VS Code host suites live in extension/test/host and run under
// @vscode/test-cli (see .vscode-test.mjs). coverage.include is the node
// report's file list; HOST_COVERAGE_DROPS mirrors the host/common entries
// so the two reports stay disjoint. The per-metric `thresholds` are the
// node-side floor for the coverage gate; host-side floors live in
// scripts/build/coverage_metrics.py.
export default defineConfig({
	test: {
		include: ["extension/test/unit/**/*.test.ts"],
		environment: "node",
		coverage: {
			provider: "v8",
			include: [
				"extension/src/host/commandProgress.ts",
				"extension/src/host/commands.ts",
				"extension/src/host/diagnosticsSignature.ts",
				"extension/src/host/documentLanguage.ts",
				"extension/src/host/engine.ts",
				"extension/src/host/executable.ts",
				"extension/src/host/fileListSignature.ts",
				"extension/src/host/fnv1a.ts",
				"extension/src/host/focusTracking.ts",
				"extension/src/host/games.ts",
				"extension/src/host/graphAvailability.ts",
				"extension/src/host/logger.ts",
				"extension/src/host/reindexSettings.ts",
				"extension/src/host/rulesManifest.ts",
				"extension/src/host/rulesSetup.ts",
				"extension/src/host/serverBlockedDialog.ts",
				"extension/src/host/trustedPaths.ts",
				"extension/src/host/watchedFiles.ts",
				"extension/src/webview/graph.ts",
			],
			reporter: ["text-summary", "html", "lcov", "json-summary"],
			reportsDirectory: "coverage-node",
			thresholds: {
				lines: 70,
				statements: 70,
				branches: 68,
				functions: 71,
			},
		},
	},
	resolve: {
		// engine.ts reaches vscode transitively (logger.ts). Swap in a tiny stub
		// so these tests stay out of the Electron host.
		alias: {
			vscode: fileURLToPath(
				new URL("./extension/test/unit/_stubs/vscode.ts", import.meta.url),
			),
		},
	},
});
