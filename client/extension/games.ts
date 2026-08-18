// Single source of truth for supported games. Everything here is pure data —
// no vscode import — so vitest owns it (see vitest.config.ts).
export interface GameDef {
	id: string;
	display: string;
	repo: string;
	// Rules commit the extension fetches. Bumped by build/rulesPins.ts, never
	// tracked live, so a push to an upstream repo can't reach users unasked.
	repoRef: string;
	exeName: string;
	// Exe lives under binaries/ in the vanilla install.
	binariesPrefix: boolean;
	// Matched against the lowercased workspace path (string = substring).
	folderHint: RegExp | string;
	// Vanilla Steam install folder names, lowercased.
	vanillaFolders: string[];
	// Games that keep `common` under a game/ subdir in the vanilla install.
	vanillaSubdir?: string;
	// A dir unique to this game, for opaque folder names.
	contentHint?: string;
}

export const RULES_MANIFEST_REVISION = 1;

// Order matters: folder hints are tried in order, so the 3-suffixed games sit
// before their 2-suffixed prefixes ("crusader kings iii" contains
// "crusader kings ii", "victoria iii" contains "victoria ii").
export const GAMES: GameDef[] = [
	{
		id: "stellaris",
		display: "Stellaris",
		repo: "https://github.com/cwtools/cwtools-stellaris-config",
		repoRef: "99147efe8072b331005b50536762dcc0b8573224", // 2026-08-05
		exeName: "stellaris",
		binariesPrefix: false,
		folderHint: /stellaris/,
		vanillaFolders: ["stellaris"],
		contentHint: "common/species_classes",
	},
	{
		id: "hoi4",
		display: "Hearts of Iron IV",
		// Overridable so the rules-sync host suite can point hoi4 at a local,
		// checked-in bare repo instead of the real network (see
		// client/test/suite/rulesSync.test.ts). Unset in every real install.
		repo:
			process.env.CWTOOLS_TEST_HOI4_REPO ||
			"https://github.com/cwtools/cwtools-hoi4-config",
		repoRef:
			process.env.CWTOOLS_TEST_HOI4_REF ||
			"ab1fda2a599ab4318d6f24ecba380e579e37006a", // 2026-08-05
		exeName: "hoi4",
		binariesPrefix: false,
		folderHint: /(hoi4|hearts)/,
		vanillaFolders: ["hearts of iron iv"],
		contentHint: "common/ai_strategy",
	},
	{
		id: "eu4",
		display: "Europa Universalis IV",
		repo: "https://github.com/cwtools/cwtools-eu4-config",
		repoRef: "a85622d368bbb7afca938ed70fdd5eda44aec769", // 2026-08-05
		exeName: "eu4",
		binariesPrefix: false,
		folderHint: /(eu4|europa)/,
		vanillaFolders: ["europa universalis iv"],
		contentHint: "common/great_projects",
	},
	{
		id: "ck3",
		display: "Crusader Kings III",
		repo: "https://github.com/cwtools/cwtools-ck3-config",
		repoRef: "27db56f995af6b73baebae43e04d044f1a0a0bbe", // 2026-08-05
		exeName: "ck3",
		binariesPrefix: true,
		folderHint: /(ck3|crusader kings iii)/,
		vanillaFolders: ["crusader kings iii"],
		vanillaSubdir: "game",
		contentHint: "common/dynasties",
	},
	{
		id: "ck2",
		display: "Crusader Kings II",
		repo: "https://github.com/cwtools/cwtools-ck2-config",
		repoRef: "aedc351934035017aff7a4429afae93dad739dd5", // 2026-08-05
		exeName: "CK2",
		binariesPrefix: false,
		folderHint: /(ck2|crusader kings ii)/,
		vanillaFolders: ["crusader kings ii"],
	},
	{
		id: "vic3",
		display: "Victoria 3",
		repo: "https://github.com/cwtools/cwtools-vic3-config",
		repoRef: "ba728e59a22e18dd590964e1c0201976852f46a8", // 2026-08-05
		exeName: "victoria3",
		binariesPrefix: true,
		folderHint: /(vic3|victoria (iii|3))/,
		vanillaFolders: ["victoria 3"],
		vanillaSubdir: "game",
	},
	{
		id: "vic2",
		display: "Victoria II",
		repo: "https://github.com/cwtools/cwtools-vic2-config",
		repoRef: "b8992c0f48a9878ffb45835fc677a6fa271899d2", // 2026-08-05
		exeName: "v2game",
		binariesPrefix: false,
		folderHint: /(vic2|victoria (ii|2))/,
		vanillaFolders: ["victoria ii", "victoria 2"],
	},
	{
		id: "imperator",
		display: "Imperator",
		repo: "https://github.com/cwtools/cwtools-ir-config",
		repoRef: "84c1d97fd36a5b8bdd0467100b2e987905808f53", // 2026-08-05
		exeName: "imperator",
		binariesPrefix: true,
		folderHint: /(imperator|rome)/,
		vanillaFolders: ["imperatorrome", "imperator"],
		vanillaSubdir: "game",
	},
	{
		id: "eu5",
		display: "Europa Universalis V",
		repo: "https://github.com/kaiser-chris/cwtools-eu5-config",
		repoRef: "7f2764a9536951dc9915c0b05509d0499408381a", // 2026-08-05
		exeName: "eu5",
		binariesPrefix: true,
		folderHint: "eu5",
		vanillaFolders: ["europa universalis v"],
		vanillaSubdir: "game",
	},
];

export interface RulesRepo {
	repo: string;
	ref: string;
}

export const LANGUAGE_REPOS: Record<string, RulesRepo> = Object.fromEntries(
	GAMES.map((g) => [g.id, { repo: g.repo, ref: g.repoRef }]),
);

export const FOLDER_HINTS: Array<[RegExp | string, string]> = GAMES.map((g) => [
	g.folderHint,
	g.id,
]);

export const CONTENT_HINTS: Array<[string, string]> = GAMES.filter(
	(g) => g.contentHint,
).map((g) => [g.contentHint!, g.id]);
