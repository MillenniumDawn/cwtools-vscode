// Single source for the host-based extension tests. Pick a config with
// `vscode-test --label <name>` (see the npm `test:*` scripts):
//
//   unit        fast suites that need the VS Code API but not the language server
//   smoke       unit + activation, in the sample workspace
//   live        the live-settings suite, in sample-live (its own workspace
//               because .vscode/settings.json attempts to set rules_folder)
//   rules-sync  activation-triggered rules sync against a local hoi4 fixture
//               (see below; its own CI step, not part of test:smoke)
//   host        full suite excl. live (slower, see below)
//
// A label selects exactly one config: @vscode/test-cli resolves `--label x`
// with config.tests.find(), so a second entry sharing a label is never run.
// Labels are therefore unique here, and `test:smoke` passes both `--label
// smoke` and `--label live` to cover the two workspaces.
//
// The hover and completion suites assert on rule-driven data, which needs the
// workspace to detect as a real game rather than the generic `paradox`
// language (no rules repo to clone). The sample workspace has no game name in
// its path, but extension/test/workspaces/stellaris/common/species_classes
// (added for #185, unrelated to detection) is Stellaris's content marker (see
// extension/src/host/games.ts), so it already detects as `stellaris` and fetches
// real rules in the background on activation. `host` needs the built server
// binary, so it is gated in ci.yml's `build` job rather than `check`; two of
// its assertions stay `test.skip` against genuine engine gaps
// (MillenniumDawn/cwtools#317, #318).
//
// rules-sync exercises the opposite gap: sample-hoi4's folder name and
// common/ai_strategy dir make it detect as hoi4, so activation's real
// resolveRulesCache/fetchRulesInBackground actually run (see rulesSetup.ts).
// games.ts and rulesManifest.ts read CWTOOLS_TEST_HOI4_REPO/_REF and
// CWTOOLS_TEST_RULES_MANIFEST_URL (below) to point that sync at the
// checked-in bare repo under extension/test/fixtures/hoi4-rules.git instead of
// the real network. It still needs a real cwtools-server binary staged at
// dist/extension/bin/server: activation's init() returns before it reaches the
// rules sync when the binary is missing, same as `smoke`/`live`. It stays
// separate from test:smoke because its fixture and network-free environment
// differ. CI runs it as its own step after the server has been staged.
//
// Coverage applies globally when `--coverage` is passed (test:coverage runs the
// `host` label, since its file list is a superset of `unit`'s and `smoke`'s
// and it's the only label that exercises modules like graphPanel.ts).

import * as path from "node:path";
import { defineConfig } from "@vscode/test-cli";

const sampleWorkspace = "./extension/test/workspaces/stellaris";
const liveWorkspace = "./extension/test/workspaces/live";
const rulesSyncWorkspace = "./extension/test/workspaces/hoi4";
const sampleFile = "./extension/test/workspaces/stellaris/events/irm.txt";
const liveSampleFile = "./extension/test/workspaces/live/events/irm.txt";
const rulesSyncSampleFile = "./extension/test/workspaces/hoi4/events/irm.txt";
const liveRulesFolder = path.resolve(
	import.meta.dirname,
	"extension/test/workspaces/live/.cwtools-test-rules",
);
const hoi4RulesFixture = path.resolve(
	import.meta.dirname,
	"extension/test/fixtures/hoi4-rules.git",
);
// Root hooks only, no tests: names the in-flight test if the host exits before
// mocha reports (#216). First in every label, since the abort has now been seen
// on `live` as well as `host`.
const abortDiagnostics =
	"./dist/extension/bin/client/test/support/abortDiagnostics.js";
const unitFiles = [
	abortDiagnostics,
	"./dist/extension/bin/client/test/host/graphTypes.test.js",
	"./dist/extension/bin/client/test/host/fileExplorer.test.js",
];
// Stops the language client to prove deactivate() completes the LSP shutdown
// handshake (#502), so it goes last in every label that lists it.
const deactivateFile =
	"./dist/extension/bin/client/test/host/deactivate.test.js";
const smokeFiles = [
	...unitFiles,
	"./dist/extension/bin/client/test/host/extension.test.js",
	deactivateFile,
];
// Live-settings fixture is isolated: its .vscode/settings.json attempts to set
// cwtools.rules_folder to .cwtools-test-rules, while the test environment
// supplies the absolute fixture path before activation. Running it in the
// shared sample workspace would pollute the host/unit suites (they expect the
// generic paradox ruleset), so it lives in extension/test/workspaces/live.
const liveFiles = [
	abortDiagnostics,
	"./dist/extension/bin/client/test/host/liveSettings.test.js",
];
const rulesSyncFiles = [
	abortDiagnostics,
	"./dist/extension/bin/client/test/host/rulesSync.test.js",
];
const hostFiles = [
	abortDiagnostics,
	"./dist/extension/bin/client/test/host/graphTypes.test.js",
	"./dist/extension/bin/client/test/host/fileExplorer.test.js",
	"./dist/extension/bin/client/test/host/extension.test.js",
	"./dist/extension/bin/client/test/host/diagnostics.test.js",
	"./dist/extension/bin/client/test/host/hover.test.js",
	"./dist/extension/bin/client/test/host/completion.test.js",
	"./dist/extension/bin/client/test/host/codeLens.test.js",
	deactivateFile,
];

// CI already renders the graph webview in software (#210's logs show the GPU
// process failing under xvfb), so ask for it explicitly and put a developer
// machine on the same path, which takes a GPU-process crash off #216's list.
const softwareRendering = "--disable-gpu";
const disableCrashReporter = "--disable-crash-reporter";

// scripts/build/hosttest.py resolves the display backend and exports the name
// it settled on. `ozone` is Electron's own headless Chromium backend: it needs
// no system package, but the flag is an undocumented Chromium detail, so it is
// opt-in (CWTOOLS_TEST_DISPLAY=ozone) rather than the Linux default.
const headless =
	process.env.CWTOOLS_TEST_DISPLAY === "ozone"
		? ["--ozone-platform=headless"]
		: [];

const base = {
	vscode: "stable",
	extensionDevelopmentPath: "dist/extension",
};

export default defineConfig({
	tests: [
		{
			...base,
			label: "unit",
			files: unitFiles,
			workspaceFolder: sampleWorkspace,
			launchArgs: [
				sampleFile,
				softwareRendering,
				disableCrashReporter,
				...headless,
			],
		},
		{
			...base,
			label: "smoke",
			files: smokeFiles,
			workspaceFolder: sampleWorkspace,
			launchArgs: [
				sampleFile,
				softwareRendering,
				disableCrashReporter,
				...headless,
			],
		},
		{
			...base,
			label: "live",
			files: liveFiles,
			workspaceFolder: liveWorkspace,
			launchArgs: [
				liveSampleFile,
				softwareRendering,
				disableCrashReporter,
				...headless,
			],
			env: {
				CWTOOLS_TEST_RULES_FOLDER: liveRulesFolder,
			},
		},
		{
			...base,
			label: "rules-sync",
			files: rulesSyncFiles,
			workspaceFolder: rulesSyncWorkspace,
			launchArgs: [
				rulesSyncSampleFile,
				softwareRendering,
				disableCrashReporter,
				...headless,
			],
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
			launchArgs: [
				sampleFile,
				softwareRendering,
				disableCrashReporter,
				"--log=debug",
				...headless,
			],
		},
	],
	coverage: {
		reporter: ["text-summary", "html", "lcov", "json-summary"],
		// c8 filters emitted JavaScript paths before it remaps source maps, so
		// these globs name the extension bundle and tsc output rather than the
		// TypeScript source tree. The bundle's source map still brings bundled
		// dependencies and Vitest-owned modules into the raw report;
		// scripts/build/coverage_summary.py filters those after remapping.
		include: [
			"**/dist/extension/bin/client/extension/extension.js",
			"**/dist/extension/bin/client/src/host/**",
			"**/dist/extension/bin/client/src/common/**",
		],
		exclude: [
			"**/dist/extension/bin/client/test/**",
			"**/dist/extension/bin/client/webview/**",
			"**/node_modules/**",
		],
	},
});
