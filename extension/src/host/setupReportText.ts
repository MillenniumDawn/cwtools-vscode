import type { GameDetection } from "./detectGame";

export const SETUP_REPORT_SCHEME = "cwtools-setup-report";

export type VanillaCacheReport =
	| { status: "unavailable"; reason: string }
	| ({ path: string; fingerprint: string } & (
			| { status: "present" | "absent" }
			| { status: "stale"; otherCaches: number }
			| { status: "versionMismatch"; fileVersion: number; expectedVersion: number }
			| { status: "unreadable"; reason: string }
	  ));

interface ScanReport {
	totalFiles: number;
	validatedFiles: number;
	filesWithErrors: number;
	totalErrors: number;
	totalWarnings: number;
	totalInfos: number;
	totalHints: number;
}

// The server's getSetupReport result, from engine/crates/lsp/src/setup_report.rs.
export interface ServerSetupReport {
	server: { version: string };
	state: { indexReady: boolean; scanInProgress: boolean };
	game: { language: string; known: boolean };
	workspace: { roots: string[]; parentMods: string[] };
	rules: {
		dir: string | null;
		dirExists: boolean;
		loaded: boolean;
		types: number;
	};
	vanilla: {
		dir: string | null;
		dirExists: boolean;
		merged: boolean;
		cache: VanillaCacheReport;
	};
	index: {
		typeInstances: number;
		types: number;
		files: number;
		variables: number;
	};
	localisation: { keys: number; languages: number };
	lastScan: ScanReport | null;
	ignore: {
		filePatterns: string[];
		directories: string[];
		errorCodes: string[];
	};
}

export type ServerOutcome =
	| { kind: "report"; report: ServerSetupReport }
	| { kind: "unavailable"; reason: string };

export interface VersionFacts {
	extensionVersion: string;
	vscodeVersion: string;
}

export interface ClientFacts extends VersionFacts {
	language: string;
	detectedBy: GameDetection["source"];
	serverExe: string | undefined;
	rules: {
		/** The `rules_folder` setting as written, undefined when unset or blank. */
		setting: string | undefined;
		/** Where the setting resolved to, existing or not. */
		settingPath: string | undefined;
		settingExists: boolean;
		managedDir: string;
		/** Directory handed to the server: the setting when it exists, else the clone. */
		dir: string;
		head: string | null;
	};
	/** The `cwtools.cache.<game>` setting, undefined when unset or blank. */
	vanillaSetting: string | undefined;
	vanillaSettingExists: boolean;
}

const DETECTED_BY: Record<ClientFacts["detectedBy"], string> = {
	markers: "workspace folder name or content markers",
	executable: "game executable found in the workspace",
	default: "nothing recognized, so the generic paradox language is used",
};

const code = (value: string): string => `\`${value}\``;

const codeList = (items: readonly string[]): string =>
	items.length > 0 ? items.map(code).join(", ") : "none";

function rulesFolderLine(rules: ClientFacts["rules"]): string {
	if (rules.setting === undefined) {
		return "- rules_folder setting: not set";
	}
	if (rules.settingExists) {
		return `- rules_folder setting: ${code(rules.setting)} (exists)`;
	}
	return `- rules_folder setting: ${code(rules.setting)} does not exist (tried ${code(rules.settingPath ?? rules.setting)}); falling back to the managed clone`;
}

function cacheLine(cache: VanillaCacheReport): string {
	switch (cache.status) {
		case "present":
			return "present";
		case "absent":
			return "absent (no cache file for this game in the cache directory)";
		case "stale":
			return `stale (${cache.otherCaches} cache file(s) for other installs or rules, none for this one)`;
		case "versionMismatch":
			return `version mismatch (file is format ${cache.fileVersion}, this server reads ${cache.expectedVersion})`;
		case "unreadable":
			return `unreadable (${cache.reason})`;
		case "unavailable":
			return `unavailable (${cache.reason})`;
	}
}

function vanillaSettingLine(client: ClientFacts): string {
	const name = `- cwtools.cache.${client.language} setting:`;
	if (client.vanillaSetting === undefined) {
		return `${name} not set`;
	}
	return `${name} ${code(client.vanillaSetting)}${client.vanillaSettingExists ? " (exists)" : " does not exist"}`;
}

function vanillaDirNote(
	client: ClientFacts,
	vanilla: ServerSetupReport["vanilla"],
): string {
	const notes: string[] = [];
	if (client.vanillaSetting === undefined) {
		notes.push("found automatically");
	} else if (client.vanillaSetting !== vanilla.dir) {
		notes.push("differs from the setting");
	}
	if (!vanilla.dirExists) {
		notes.push("does not exist");
	}
	return notes.length > 0 ? ` (${notes.join(", ")})` : "";
}

function yesNo(value: boolean): string {
	return value ? "yes" : "no";
}

function serverLines(client: ClientFacts, report: ServerSetupReport): string[] {
	const lines: string[] = [];
	const { rules, vanilla, index, localisation, lastScan, ignore } = report;

	lines.push("", "## Server state");
	lines.push(`- Index ready: ${yesNo(report.state.indexReady)}`);
	lines.push(`- Scan in progress: ${yesNo(report.state.scanInProgress)}`);
	lines.push(
		`- Game the server runs: ${report.game.language}${report.game.known ? "" : " (not a known game)"}`,
	);

	lines.push("", "## Workspace");
	for (const root of report.workspace.roots) {
		lines.push(`- Root: ${code(root)}`);
	}
	lines.push(`- Parent mods: ${codeList(report.workspace.parentMods)}`);

	lines.push("", "## Rules loaded by the server");
	if (rules.dir) {
		lines.push(
			`- Directory: ${code(rules.dir)}${rules.dirExists ? "" : " (does not exist)"}`,
		);
	} else {
		lines.push("- Directory: none given to the server");
	}
	lines.push(`- Loaded: ${rules.loaded ? `yes, ${rules.types} types` : "no"}`);
	if (rules.dir && rules.dir !== client.rules.dir) {
		lines.push(
			`- Differs from the directory the extension chose (${code(client.rules.dir)})`,
		);
	}

	lines.push("", "## Base game", vanillaSettingLine(client));
	if (vanilla.dir) {
		lines.push(`- Directory: ${code(vanilla.dir)}${vanillaDirNote(client, vanilla)}`);
	} else {
		lines.push("- Directory: none set or found");
	}
	lines.push(`- Index merged: ${yesNo(vanilla.merged)}`);
	lines.push(`- Cache: ${cacheLine(vanilla.cache)}`);
	if (vanilla.cache.status !== "unavailable") {
		lines.push(`- Cache file: ${code(vanilla.cache.path)}`);
		lines.push(`- Cache fingerprint: ${code(vanilla.cache.fingerprint)}`);
	}

	lines.push("", "## Index");
	lines.push(`- Type instances: ${index.typeInstances} across ${index.types} types`);
	lines.push(`- Indexed files: ${index.files}`);
	lines.push(`- Variables: ${index.variables}`);
	lines.push(
		`- Localisation keys: ${localisation.keys} in ${localisation.languages} languages`,
	);

	lines.push("", "## Last scan");
	if (lastScan) {
		lines.push(
			`- Validated ${lastScan.validatedFiles} of ${lastScan.totalFiles} files; ${lastScan.filesWithErrors} with errors`,
			`- ${lastScan.totalErrors} errors, ${lastScan.totalWarnings} warnings, ${lastScan.totalInfos} infos, ${lastScan.totalHints} hints`,
		);
	} else {
		lines.push("- No scan has finished yet");
	}

	lines.push("", "## Ignore lists", "- Engine defaults apply as well");
	lines.push(`- File patterns: ${codeList(ignore.filePatterns)}`);
	lines.push(`- Directories: ${codeList(ignore.directories)}`);
	lines.push(`- Error codes: ${codeList(ignore.errorCodes)}`);
	return lines;
}

function reportHeader(facts: VersionFacts): string[] {
	return [
		"# CWTools setup report",
		"",
		"## Extension",
		`- CWTools: ${facts.extensionVersion}`,
		`- VS Code: ${facts.vscodeVersion}`,
	];
}

// For a workspace where activation stopped before it chose a game or a server.
export function renderNotStartedReport(
	facts: VersionFacts,
	reason: string,
	folders: readonly string[],
): string {
	const lines = [
		...reportHeader(facts),
		"",
		"## Not started",
		`- Reason: ${reason}`,
		`- Workspace folders: ${codeList(folders)}`,
	];
	return `${lines.join("\n")}\n`;
}

export function renderSetupReport(
	client: ClientFacts,
	server: ServerOutcome,
): string {
	const { rules } = client;
	const lines = reportHeader(client);

	lines.push(
		`- Server binary: ${client.serverExe === undefined ? "not found" : code(client.serverExe)}`,
	);
	if (server.kind === "report") {
		lines.push(
			`- Server: running, version ${server.report.server.version}`,
		);
	} else {
		lines.push(`- Server: not available (${server.reason})`);
	}

	lines.push("", "## Game");
	lines.push(`- Game: ${client.language}`);
	lines.push(`- Detected by: ${DETECTED_BY[client.detectedBy]}`);

	lines.push("", "## Rules chosen by the extension");
	lines.push(`- Directory: ${code(rules.dir)}`);
	lines.push(
		`- Source: ${rules.settingExists ? "rules_folder setting" : "managed clone"}`,
	);
	lines.push(rulesFolderLine(rules));
	lines.push(`- Managed clone: ${code(rules.managedDir)}`);
	lines.push(
		`- Git HEAD: ${rules.head === null ? "none found (not a git checkout, or git is not available)" : rules.head}`,
	);

	if (server.kind === "report") {
		lines.push(...serverLines(client, server.report));
	} else {
		lines.push("", "## Base game", vanillaSettingLine(client));
	}
	return `${lines.join("\n")}\n`;
}
