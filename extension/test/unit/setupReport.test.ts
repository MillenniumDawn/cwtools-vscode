import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import {
	renderSetupReport,
	SETUP_REPORT_SCHEME,
	type ClientFacts,
	type ServerSetupReport,
	type VanillaCacheReport,
} from "../../src/host/setupReportText";

const managedDir = "/cache/.cwtools/hoi4";

function facts(rules: Partial<ClientFacts["rules"]> = {}): ClientFacts {
	return {
		extensionVersion: "4.0.5",
		vscodeVersion: "1.95.0",
		language: "hoi4",
		detectedBy: "markers",
		serverExe: "/ext/bin/cwtools-server",
		vanillaSetting: undefined,
		vanillaSettingExists: false,
		rules: {
			setting: undefined,
			settingPath: undefined,
			settingExists: false,
			managedDir,
			dir: managedDir,
			head: null,
			...rules,
		},
	};
}

const serverReport: ServerSetupReport = {
	server: { version: "3.4.0" },
	state: { indexReady: true, scanInProgress: false },
	game: { language: "hoi4", known: true },
	workspace: { roots: ["/mods/md"], parentMods: [] },
	rules: { dir: managedDir, dirExists: true, loaded: true, types: 120 },
	vanilla: {
		dir: "/games/hoi4",
		dirExists: true,
		merged: true,
		cache: { status: "present", path: "/cache/vanilla-hoi4-x.cwv", fingerprint: "fp" },
	},
	index: { typeInstances: 5000, types: 80, files: 900, variables: 40 },
	localisation: { keys: 7000, languages: 1 },
	lastScan: {
		totalFiles: 10,
		validatedFiles: 9,
		filesWithErrors: 2,
		totalErrors: 3,
		totalWarnings: 4,
		totalInfos: 5,
		totalHints: 6,
	},
	ignore: { filePatterns: ["*.bak"], directories: ["scratch"], errorCodes: [] },
};

function render(
	client: ClientFacts = facts(),
	report: ServerSetupReport = serverReport,
): string {
	return renderSetupReport(client, { kind: "report", report });
}

const cacheFile = { path: "/cache/vanilla-hoi4-x.cwv", fingerprint: "fp" };

function withCache(cache: VanillaCacheReport): string {
	return render(facts(), {
		...serverReport,
		vanilla: { ...serverReport.vanilla, cache },
	});
}

suite("setup report text", () => {
	test("names the resolved rules directory, the game and how it was detected", () => {
		const text = render();
		assert.match(text, /- Directory: `\/cache\/\.cwtools\/hoi4`/);
		assert.match(text, /- Source: managed clone/);
		assert.match(text, /- Game: hoi4/);
		assert.match(text, /- Detected by: workspace folder name or content markers/);
		assert.match(text, /- Server: running, version 3\.4\.0/);
		assert.match(text, /- Server binary: `\/ext\/bin\/cwtools-server`/);
	});

	test("says so when rules_folder points at a path that does not exist", () => {
		const text = render(
			facts({ setting: "~/rules", settingPath: "/home/u/rules" }),
		);
		assert.match(
			text,
			/- rules_folder setting: `~\/rules` does not exist \(tried `\/home\/u\/rules`\); falling back to the managed clone/,
		);
		assert.match(text, /- Source: managed clone/);
	});

	test("reports rules_folder as the source when it exists", () => {
		const text = render(
			facts({
				setting: "/my/rules",
				settingPath: "/my/rules",
				settingExists: true,
				dir: "/my/rules",
				head: "a".repeat(40),
			}),
			{ ...serverReport, rules: { ...serverReport.rules, dir: "/my/rules" } },
		);
		assert.match(text, /- rules_folder setting: `\/my\/rules` \(exists\)/);
		assert.match(text, /- Source: rules_folder setting/);
		assert.match(text, new RegExp(`- Git HEAD: ${"a".repeat(40)}`));
		assert.doesNotMatch(text, /Differs from/);
	});

	test("says when rules_folder is unset and no git HEAD was found", () => {
		const text = render();
		assert.match(text, /- rules_folder setting: not set/);
		assert.match(text, /- Git HEAD: none found \(not a git checkout, or git is not available\)/);
	});

	test("flags a server that loaded rules from another directory", () => {
		const text = render(facts(), {
			...serverReport,
			rules: { dir: "/elsewhere", dirExists: false, loaded: false, types: 0 },
		});
		assert.match(text, /- Directory: `\/elsewhere` \(does not exist\)/);
		assert.match(text, /- Loaded: no/);
		assert.match(text, /Differs from the directory the extension chose/);
	});

	const cacheLines: Array<[string, VanillaCacheReport, RegExp]> = [
		["present", { ...cacheFile, status: "present" }, /- Cache: present$/m],
		["absent", { ...cacheFile, status: "absent" }, /- Cache: absent \(no cache file/m],
		["stale", { ...cacheFile, status: "stale", otherCaches: 2 }, /- Cache: stale \(2 cache file\(s\)/m],
		[
			"version mismatched",
			{ ...cacheFile, status: "versionMismatch", fileVersion: 14, expectedVersion: 15 },
			/- Cache: version mismatch \(file is format 14, this server reads 15\)/m,
		],
		["unreadable", { ...cacheFile, status: "unreadable", reason: "denied" }, /- Cache: unreadable \(denied\)/m],
		[
			"unavailable",
			{ status: "unavailable", reason: "no base-game directory is set or found" },
			/- Cache: unavailable \(no base-game directory is set or found\)/m,
		],
	];
	for (const [name, cache, expected] of cacheLines) {
		test(`describes a ${name} base-game cache`, () => {
			assert.match(withCache(cache), expected);
		});
	}

	test("shows a base-game setting that the server is using", () => {
		const text = render({
			...facts(),
			vanillaSetting: "/games/hoi4",
			vanillaSettingExists: true,
		});
		assert.match(text, /- cwtools\.cache\.hoi4 setting: `\/games\/hoi4` \(exists\)/);
		assert.match(text, /- Directory: `\/games\/hoi4`$/m);
		assert.match(text, /- Cache file: `\/cache\/vanilla-hoi4-x\.cwv`/);
	});

	test("says a base-game directory was found automatically when the setting is unset", () => {
		const text = render();
		assert.match(text, /- cwtools\.cache\.hoi4 setting: not set/);
		assert.match(text, /- Directory: `\/games\/hoi4` \(found automatically\)/);
	});

	test("says so when the base-game setting points at a path that does not exist", () => {
		const text = render({ ...facts(), vanillaSetting: "/gone/hoi4" });
		assert.match(text, /- cwtools\.cache\.hoi4 setting: `\/gone\/hoi4` does not exist/);
		assert.match(text, /- Directory: `\/games\/hoi4` \(differs from the setting\)/);
	});

	test("lists index sizes, the last scan and the ignore lists", () => {
		const text = render();
		assert.match(text, /- Type instances: 5000 across 80 types/);
		assert.match(text, /- Localisation keys: 7000 in 1 languages/);
		assert.match(text, /- Validated 9 of 10 files; 2 with errors/);
		assert.match(text, /- 3 errors, 4 warnings, 5 infos, 6 hints/);
		assert.match(text, /- File patterns: `\*\.bak`/);
		assert.match(text, /- Directories: `scratch`/);
		assert.match(text, /- Error codes: none/);
	});

	test("says no scan has finished when the server has no summary", () => {
		assert.match(
			render(facts(), { ...serverReport, lastScan: null }),
			/- No scan has finished yet/,
		);
	});

	test("still describes the setup when the server cannot answer", () => {
		const text = renderSetupReport(
			{
				...facts(),
				serverExe: undefined,
				vanillaSetting: "/games/hoi4",
				vanillaSettingExists: true,
			},
			{ kind: "unavailable", reason: "the language server is not running" },
		);
		assert.match(text, /- Server binary: not found/);
		assert.match(text, /- Server: not available \(the language server is not running\)/);
		assert.match(text, /- Directory: `\/cache\/\.cwtools\/hoi4`/);
		assert.match(text, /- cwtools\.cache\.hoi4 setting: `\/games\/hoi4` \(exists\)/);
		assert.doesNotMatch(text, /## Index|## Last scan|## Ignore lists/);
	});

});

suite("setup report manifest", () => {
	const repoRoot = path.resolve(__dirname, "../../..");
	const manifest = JSON.parse(
		fs.readFileSync(path.join(repoRoot, "extension", "package", "package.json"), "utf8"),
	) as {
		contributes: {
			commands: Array<{ command: string; title: string; icon?: string }>;
			menus: Record<string, Array<{ command: string; when?: string; group?: string }>>;
		};
	};

	test("copy is one click away on the report's editor title only", () => {
		const entries = manifest.contributes.menus["editor/title"].filter(
			(entry) => entry.command === "cwtools.copySetupReport",
		);
		assert.strictEqual(entries.length, 1);
		assert.strictEqual(entries[0].when, `resourceScheme == ${SETUP_REPORT_SCHEME}`);
		assert.strictEqual(entries[0].group, "navigation");
		const copy = manifest.contributes.commands.find(
			(entry) => entry.command === "cwtools.copySetupReport",
		);
		assert.ok(copy?.icon, "a title button needs an icon");
	});

	test("both commands are contributed and gated in the palette", () => {
		const contributed = manifest.contributes.commands.map((entry) => entry.command);
		assert.ok(contributed.includes("cwtools.showSetupReport"));
		const palette = manifest.contributes.menus.commandPalette;
		assert.strictEqual(
			palette.find((entry) => entry.command === "cwtools.showSetupReport")?.when,
			"cwtoolsEnabled",
		);
		assert.strictEqual(
			palette.find((entry) => entry.command === "cwtools.copySetupReport")?.when,
			`resourceScheme == ${SETUP_REPORT_SCHEME}`,
		);
	});
});
