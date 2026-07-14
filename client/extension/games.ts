// Single source of truth for supported games. Everything here is pure data —
// no vscode import — so vitest owns it (see vitest.config.ts).
export interface GameDef {
	id: string;
	display: string;
	repo: string;
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

// Order matters: folder hints are tried in order, so the 3-suffixed games sit
// before their 2-suffixed prefixes ("crusader kings iii" contains
// "crusader kings ii", "victoria iii" contains "victoria ii").
export const GAMES: GameDef[] = [
	{
		id: 'stellaris',
		display: 'Stellaris',
		repo: 'https://github.com/cwtools/cwtools-stellaris-config',
		exeName: 'stellaris',
		binariesPrefix: false,
		folderHint: /stellaris/,
		vanillaFolders: ['stellaris'],
		contentHint: 'common/species_classes',
	},
	{
		id: 'hoi4',
		display: 'Hearts of Iron IV',
		repo: 'https://github.com/cwtools/cwtools-hoi4-config',
		exeName: 'hoi4',
		binariesPrefix: false,
		folderHint: /(hoi4|hearts)/,
		vanillaFolders: ['hearts of iron iv'],
		contentHint: 'common/ai_strategy',
	},
	{
		id: 'eu4',
		display: 'Europa Universalis IV',
		repo: 'https://github.com/cwtools/cwtools-eu4-config',
		exeName: 'eu4',
		binariesPrefix: false,
		folderHint: /(eu4|europa)/,
		vanillaFolders: ['europa universalis iv'],
		contentHint: 'common/great_projects',
	},
	{
		id: 'ck3',
		display: 'Crusader Kings III',
		repo: 'https://github.com/cwtools/cwtools-ck3-config',
		exeName: 'ck3',
		binariesPrefix: true,
		folderHint: /(ck3|crusader kings iii)/,
		vanillaFolders: ['crusader kings iii'],
		vanillaSubdir: 'game',
		contentHint: 'common/dynasties',
	},
	{
		id: 'ck2',
		display: 'Crusader Kings II',
		repo: 'https://github.com/cwtools/cwtools-ck2-config',
		exeName: 'CK2',
		binariesPrefix: false,
		folderHint: /(ck2|crusader kings ii)/,
		vanillaFolders: ['crusader kings ii'],
	},
	{
		id: 'vic3',
		display: 'Victoria 3',
		repo: 'https://github.com/cwtools/cwtools-vic3-config',
		exeName: 'victoria3',
		binariesPrefix: true,
		folderHint: /(vic3|victoria (iii|3))/,
		vanillaFolders: ['victoria 3'],
		vanillaSubdir: 'game',
	},
	{
		id: 'vic2',
		display: 'Victoria II',
		repo: 'https://github.com/cwtools/cwtools-vic2-config',
		exeName: 'v2game',
		binariesPrefix: false,
		folderHint: /(vic2|victoria (ii|2))/,
		vanillaFolders: ['victoria ii', 'victoria 2'],
	},
	{
		id: 'imperator',
		display: 'Imperator',
		repo: 'https://github.com/cwtools/cwtools-ir-config',
		exeName: 'imperator',
		binariesPrefix: true,
		folderHint: /(imperator|rome)/,
		vanillaFolders: ['imperatorrome', 'imperator'],
		vanillaSubdir: 'game',
	},
	{
		id: 'eu5',
		display: 'Europa Universalis V',
		repo: 'https://github.com/kaiser-chris/cwtools-eu5-config',
		exeName: 'eu5',
		binariesPrefix: true,
		folderHint: 'eu5',
		vanillaFolders: ['europa universalis v'],
		vanillaSubdir: 'game',
	},
];

export const LANGUAGE_REPOS: Record<string, string> =
	Object.fromEntries(GAMES.map(g => [g.id, g.repo]));

export const FOLDER_HINTS: Array<[RegExp | string, string]> =
	GAMES.map(g => [g.folderHint, g.id]);

export const CONTENT_HINTS: Array<[string, string]> =
	GAMES.filter(g => g.contentHint).map(g => [g.contentHint!, g.id]);
