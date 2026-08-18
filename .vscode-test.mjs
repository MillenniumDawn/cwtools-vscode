// Single source for the host-based extension tests. Pick a config with
// `vscode-test --label <name>` (see the npm `test:*` scripts):
//
//   unit        fast suites that need the VS Code API but not the language server
//   smoke       unit + activation, in the sample workspace
//   live        the live-settings suite, in sample-live (its own workspace
//               because .vscode/settings.json pins rules_folder)
//   rules-sync  activation-triggered rules sync against a local hoi4 fixture
//               (see below; not part of test:smoke)
//   host        full suite excl. live (slower, see below)
//
// A label selects exactly one config: @vscode/test-cli resolves `--label x`
// with config.tests.find(), so a second entry sharing a label is never run.
// Labels are therefore unique here, and `test:smoke` passes both `--label
// smoke` and `--label live` to cover the two workspaces.
//
// The hover and completion suites assert on rule-driven data. The sample
// workspace has no game name in its path and no game-specific content dir, so
// it detects as the generic `paradox` language, which has no rules repo to
// clone; those suites then see only word-completion fallback and empty hovers
// and fail. CI gates on `smoke` (which still boots the server binary and checks
// activation) and `host` stays the local full run until the fixture is made
// identifiable as a game.
//
// rules-sync exercises the opposite gap: sample-hoi4's folder name and
// common/ai_strategy dir make it detect as hoi4, so activation's real
// resolveRulesCache/fetchRulesInBackground actually run (see rulesSetup.ts).
// games.ts and rulesManifest.ts read CWTOOLS_TEST_HOI4_REPO/_REF and
// CWTOOLS_TEST_RULES_MANIFEST_URL (below) to point that sync at the
// checked-in bare repo under client/test/fixtures/hoi4-rules.git instead of
// the real network. It still needs a real cwtools-server binary staged at
// release/bin/server: activation's init() returns before it ever reaches the
// rules sync when the binary is missing, same as `smoke`/`live`. Left out of
// test:smoke for now rather than folded in unasked; wiring it into CI's smoke
// step (pr.yml) is a separate call.
//
// Coverage applies globally when `--coverage` is passed (test:coverage runs the
// `unit` label).

import * as path from "node:path";
import { defineConfig } from "@vscode/test-cli";

const sampleWorkspace = "./client/test/sample";
const liveWorkspace = "./client/test/sample-live";
const rulesSyncWorkspace = "./client/test/sample-hoi4";
const sampleFile = "./client/test/sample/events/irm.txt";
const liveSampleFile = "./client/test/sample-live/events/irm.txt";
const rulesSyncSampleFile = "./client/test/sample-hoi4/events/irm.txt";
const hoi4RulesFixture = path.resolve(
	import.meta.dirname,
	"client/test/fixtures/hoi4-rules.git",
);
const unitFiles = [
	"./release/bin/client/test/suite/graphTypes.test.js",
	"./release/bin/client/test/suite/fileExplorer.test.js",
];
// Live-settings fixture is isolated: its .vscode/settings.json pins
// cwtools.rules_folder to .cwtools-test-rules, which replaces the ruleset.
// Running it in the shared sample workspace would pollute the host/unit
// suites (they expect the generic paradox ruleset), so it lives in
// sample-live with its own workspace.
const smokeFiles = [
	...unitFiles,
	"./release/bin/client/test/suite/extension.test.js",
];
const liveFiles = ["./release/bin/client/test/suite/liveSettings.test.js"];
const rulesSyncFiles = ["./release/bin/client/test/suite/rulesSync.test.js"];
const hostFiles = [
	"./release/bin/client/test/suite/graphTypes.test.js",
	"./release/bin/client/test/suite/fileExplorer.test.js",
	"./release/bin/client/test/suite/extension.test.js",
	"./release/bin/client/test/suite/hover.test.js",
	"./release/bin/client/test/suite/completion.test.js",
];

const base = {
	vscode: "stable",
	extensionDevelopmentPath: "release",
};

export default defineConfig({
	tests: [
		{
			...base,
			label: "unit",
			files: unitFiles,
			workspaceFolder: sampleWorkspace,
			launchArgs: [sampleFile],
		},
		{
			...base,
			label: "smoke",
			files: smokeFiles,
			workspaceFolder: sampleWorkspace,
			launchArgs: [sampleFile],
		},
		{
			...base,
			label: "live",
			files: liveFiles,
			workspaceFolder: liveWorkspace,
			launchArgs: [liveSampleFile],
		},
		{
			...base,
			label: "rules-sync",
			files: rulesSyncFiles,
			workspaceFolder: rulesSyncWorkspace,
			launchArgs: [rulesSyncSampleFile],
			env: {
				CWTOOLS_TEST_HOI4_REPO: hoi4RulesFixture,
				CWTOOLS_TEST_HOI4_REF: "3f03757a6f15565f763434e5752021c3ba8c0c3e",
				CWTOOLS_TEST_RULES_MANIFEST_URL: "http://127.0.0.1:1/rules-pins.json",
			},
		},
		{
			...base,
			label: "host",
			files: hostFiles,
			workspaceFolder: sampleWorkspace,
			launchArgs: [sampleFile],
		},
	],
	coverage: {
		reporter: ["text-summary", "html", "lcov", "json-summary"],
		// Intent: only the hand-written client source. vscode-test instruments
		// every loaded file though, and doesn't reliably honor these globs: it
		// leaks node_modules into the raw report (semver, vscode-jsonrpc, ...)
		// and still reports engine.ts / executable.ts / games.ts / rulesManifest.ts
		// / rulesSetup.ts even when excluded. Those are owned by the node unit
		// tests (vitest, see vitest.config.ts);
		// build/coverage-summary.ts is what actually drops node_modules and the
		// vitest-owned files from the rendered report and recomputes the totals.
		// The excludes below are kept as declared intent.
		include: ["**/client/extension/**", "**/client/common/**"],
		exclude: [
			"**/client/extension/engine.ts",
			"**/client/extension/executable.ts",
			"**/client/extension/games.ts",
			"**/client/extension/rulesManifest.ts",
			"**/client/extension/rulesSetup.ts",
			"**/client/test/**",
			"**/client/webview/**",
			"**/node_modules/**",
		],
	},
});
