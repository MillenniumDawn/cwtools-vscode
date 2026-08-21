// Refresh the reviewed runtime manifest and bundled fallback pins from each
// upstream default branch. Run via `tsx build/rulesPins.ts`, and weekly by
// .github/workflows/rules-pins.yml. The manifest moves only through a reviewed
// PR, and clients still fetch each listed commit by its exact SHA.

import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { GAMES, RULES_MANIFEST_REVISION } from "../extension/src/host/games";
import { parseRulesManifest } from "../extension/src/host/rulesManifest";
import { extensionHostRoot, repoRoot } from "./paths";

const gamesFile = path.join(extensionHostRoot, "games.ts");
const manifestFile = path.join(repoRoot, "rules-pins.json");

const today = new Date().toISOString().slice(0, 10);
let source = fs.readFileSync(gamesFile, "utf8");

function readManifest() {
	try {
		return parseRulesManifest(
			JSON.parse(fs.readFileSync(manifestFile, "utf8")) as unknown,
		);
	} catch (err) {
		throw Object.assign(new Error(`could not read ${manifestFile}`), {
			cause: err,
		});
	}
}

const manifest = readManifest();
if (manifest.revision !== RULES_MANIFEST_REVISION) {
	throw new Error(
		`${manifestFile} revision ${manifest.revision} does not match games.ts revision ${RULES_MANIFEST_REVISION}`,
	);
}
for (const game of GAMES) {
	if (manifest.pins[game.id] !== game.repoRef) {
		throw new Error(`${game.id}: manifest pin does not match games.ts`);
	}
}

const bumped: string[] = [];
for (const game of GAMES) {
	const head = execFileSync("git", ["ls-remote", game.repo, "HEAD"], {
		encoding: "utf8",
	}).split(/\s/)[0];
	if (!/^[0-9a-f]{40}$/.test(head)) {
		throw new Error(
			`${game.id}: git ls-remote ${game.repo} returned no commit`,
		);
	}
	if (head === game.repoRef) continue;
	const pin = `repoRef: '${game.repoRef}', // `;
	const pinStart = source.indexOf(pin);
	if (pinStart === -1) {
		throw new Error(
			`${game.id}: no pin line for ${game.repoRef} in ${gamesFile}`,
		);
	}
	const pinEnd = source.indexOf("\n", pinStart);
	source =
		source.slice(0, pinStart) +
		`repoRef: '${head}', // ${today}` +
		source.slice(pinEnd === -1 ? source.length : pinEnd);
	manifest.pins[game.id] = head;
	// Compare links, so whoever reviews the bump can read what upstream changed.
	bumped.push(
		`- \`${game.id}\` ${game.repo}/compare/${game.repoRef}...${head}`,
	);
}

if (bumped.length === 0) {
	console.log("rules pins are already current");
} else {
	const nextRevision = manifest.revision + 1;
	const revision = /export const RULES_MANIFEST_REVISION = \d+;/;
	if (!revision.test(source)) {
		throw new Error(`no rules manifest revision in ${gamesFile}`);
	}
	source = source.replace(
		revision,
		`export const RULES_MANIFEST_REVISION = ${nextRevision};`,
	);
	manifest.revision = nextRevision;
	fs.writeFileSync(gamesFile, source);
	fs.writeFileSync(manifestFile, JSON.stringify(manifest, null, "\t") + "\n");
	for (const line of bumped) console.log(line);
}
