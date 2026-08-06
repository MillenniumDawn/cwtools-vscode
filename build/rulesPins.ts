// Refresh the pinned rules commits in client/extension/games.ts from each
// upstream default branch. Run via `tsx build/rulesPins.ts`, and weekly by
// .github/workflows/rules-pins.yml. The pins are what every install fetches,
// so they only ever move through a reviewed PR.

import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { GAMES } from "../client/extension/games";

const gamesFile = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	"..",
	"client",
	"extension",
	"games.ts",
);

const today = new Date().toISOString().slice(0, 10);
let source = fs.readFileSync(gamesFile, "utf8");
const bumped: string[] = [];

for (const game of GAMES) {
	const head = execFileSync("git", ["ls-remote", game.repo, "HEAD"], {
		encoding: "utf8",
	}).split(/\s/)[0];
	if (!/^[0-9a-f]{40}$/.test(head)) {
		throw new Error(`${game.id}: git ls-remote ${game.repo} returned no commit`);
	}
	if (head === game.repoRef) continue;
	const pin = new RegExp(`repoRef: '${game.repoRef}', // \\d{4}-\\d{2}-\\d{2}`);
	if (!pin.test(source)) {
		throw new Error(`${game.id}: no pin line for ${game.repoRef} in ${gamesFile}`);
	}
	source = source.replace(pin, `repoRef: '${head}', // ${today}`);
	// Compare links, so whoever reviews the bump can read what upstream changed.
	bumped.push(`- \`${game.id}\` ${game.repo}/compare/${game.repoRef}...${head}`);
}

if (bumped.length === 0) {
	console.log("rules pins are already current");
} else {
	fs.writeFileSync(gamesFile, source);
	for (const line of bumped) console.log(line);
}
