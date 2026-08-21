// Build and package the extension from checked-in inputs under extension/package.
// Generated extension files go under dist/extension and packaged vsixes under
// artifacts/vsix. Run via `tsx build/build.ts <command>`.
//
// The Rust server builds from the in-repo engine workspace by default; set
// CWTOOLS_RUST_WORKSPACE to build from another checkout.

import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { pathToFileURL } from "node:url";
import { releaseNotes, topChangelogVersion } from "./changelog";
import {
	artifactsRoot,
	engineRoot,
	extensionDistRoot,
	extensionDocsRoot,
	extensionPackageRoot,
	extensionTestRoot,
	extensionWebviewRoot,
	repoRoot,
	vsixRoot,
} from "./paths";

const extensionDir = extensionDistRoot;
const vsixDir = vsixRoot;

const isWindows = process.platform === "win32";

function run(
	cmd: string,
	args: string[],
	opts: { cwd?: string; env?: NodeJS.ProcessEnv } = {},
): void {
	const display = [cmd, ...args].join(" ");
	console.log(`> ${display}`);
	const r = spawnSync(cmd, args, {
		cwd: opts.cwd ?? repoRoot,
		env: opts.env ?? process.env,
		stdio: "inherit",
		shell: isWindows, // npx/cargo resolve via .cmd shims on Windows
	});
	if (r.status !== 0) {
		throw new Error(`command failed (${r.status ?? r.signal}): ${display}`);
	}
}

/** Like run(), but returns the exit code instead of throwing on failure. */
function runOrNull(
	cmd: string,
	args: string[],
	opts: { cwd?: string; env?: NodeJS.ProcessEnv } = {},
): number | null {
	const r = spawnSync(cmd, args, {
		cwd: opts.cwd ?? repoRoot,
		env: opts.env ?? process.env,
		stdio: "ignore",
		shell: isWindows,
	});
	return r.status;
}

// --- Rust server -----------------------------------------------------------

function rustWorkspace(): string {
	const fromEnv = process.env.CWTOOLS_RUST_WORKSPACE;
	if (fromEnv && fromEnv.trim()) return path.resolve(repoRoot, fromEnv);
	return engineRoot;
}

function buildAndDeployRustServer(): void {
	const workspace = rustWorkspace();
	run("cargo", ["build", "--release", "-p", "cwtools_lsp"], { cwd: workspace });

	const binName = isWindows ? "cwtools-server.exe" : "cwtools-server";
	const built = path.join(workspace, "target/release", binName);
	if (!fs.existsSync(built)) {
		throw new Error(
			`Rust server binary not found at '${built}' after build. Check the crate name/target, ` +
				`or point CWTOOLS_RUST_WORKSPACE at the right engine checkout (currently '${workspace}').`,
		);
	}

	// Deploy to the path the client loads first. Clean it so stale binaries
	// don't linger next to the fresh one.
	const outDir = path.join(extensionDir, "bin/server/cwtools-server");
	fs.rmSync(outDir, { recursive: true, force: true });
	fs.mkdirSync(outDir, { recursive: true });
	const dest = path.join(outDir, binName);
	fs.copyFileSync(built, dest);
	if (!isWindows) fs.chmodSync(dest, 0o755);
}

// --- Client + docs ---------------------------------------------------------

function buildClient(): void {
	run("npm", ["run", "compile:code"]);
}

function copyPackageInputs(): void {
	copyDir(extensionPackageRoot, extensionDir);
}

function copyDocs(): void {
	fs.copyFileSync(
		path.join(extensionDocsRoot, "README.md"),
		path.join(extensionDir, "README.md"),
	);
	for (const f of ["LICENSE.md", "CHANGELOG.md"]) {
		fs.copyFileSync(path.join(repoRoot, f), path.join(extensionDir, f));
	}
}

function copyDir(src: string, dest: string): void {
	fs.mkdirSync(dest, { recursive: true });
	for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
		const s = path.join(src, entry.name);
		const d = path.join(dest, entry.name);
		if (entry.isDirectory()) copyDir(s, d);
		else fs.copyFileSync(s, d);
	}
}

function copyWebviewCss(): void {
	const dest = path.join(extensionDir, "bin/client/webview");
	fs.mkdirSync(dest, { recursive: true });
	for (const f of fs.readdirSync(extensionWebviewRoot)) {
		if (f.endsWith(".css"))
			fs.copyFileSync(path.join(extensionWebviewRoot, f), path.join(dest, f));
	}
}

function copyTestSamples(): void {
	copyDir(
		path.join(extensionTestRoot, "workspaces", "stellaris"),
		path.join(extensionDir, "bin/client/test/workspaces/stellaris"),
	);
}

function cleanExtensionDist(): void {
	fs.rmSync(extensionDir, { recursive: true, force: true });
}

function assembleClient(): void {
	copyPackageInputs();
	buildClient();
	copyDocs();
	copyWebviewCss();
	copyTestSamples();
}

// --- Packaging -------------------------------------------------------------

// VS Code's platform ids, keyed by the server-binary directory names the
// release matrix stages. A platform-specific vsix carries one binary instead
// of all of them, so a download is a third the size; the Marketplace serves
// the matching one and falls back to the universal build for anything else.
const VSIX_TARGETS: Record<string, string> = {
	"win-x64": "win32-x64",
	"linux-x64": "linux-x64",
	"linux-arm64": "linux-arm64",
	"osx-x64": "darwin-x64",
	"osx-arm64": "darwin-arm64",
};

const serverBinDir = path.join(extensionDir, "bin/server/cwtools-server");

function packageVsix(target?: string): string[] {
	// The client is bundled with esbuild, so node_modules is excluded from the
	// vsix (see extension/package/.vscodeignore). --no-dependencies stops vsce
	// from trying to resolve/include them.
	const args = ["--no-install", "vsce", "package", "--no-dependencies"];
	if (target) args.push("--target", target);
	run("npx", args, { cwd: extensionDir });
	fs.mkdirSync(vsixDir, { recursive: true });
	const packaged: string[] = [];
	for (const f of fs.readdirSync(extensionDir)) {
		if (f.endsWith(".vsix")) {
			const dest = path.join(vsixDir, f);
			fs.renameSync(path.join(extensionDir, f), dest);
			packaged.push(dest);
		}
	}
	return packaged;
}

// Platform subdirectories currently staged under bin/server/cwtools-server.
// A local `quick` build drops the binary straight in that folder instead, in
// which case there is nothing to split and we package a single vsix.
function stagedPlatforms(): string[] {
	if (!fs.existsSync(serverBinDir)) return [];
	return fs
		.readdirSync(serverBinDir, { withFileTypes: true })
		.filter((e) => e.isDirectory() && e.name in VSIX_TARGETS)
		.map((e) => e.name)
		.sort();
}

// One vsix per staged platform, then a universal one carrying every binary as
// the fallback for platforms with no dedicated build (win-arm64, older macOS).
// Each pass leaves only the target platform's directory in place, so vsce can't
// sweep the others in.
//
// The holding/restore dance is split out so it can be unit-tested against a
// real temp dir with an injected packageOne (no shelling out to vsce).
// packageOne(platform) packages one per-platform vsix; packageOne(undefined)
// packages the universal fallback after the full set is restored.
export function runPlatformPackaging(
	serverBinDir: string,
	holding: string,
	platforms: string[],
	packageOne: (platform: string | undefined) => string[],
): string[] {
	fs.rmSync(holding, { recursive: true, force: true });
	fs.mkdirSync(path.dirname(holding), { recursive: true });
	fs.renameSync(serverBinDir, holding);

	const vsixes: string[] = [];
	let restored = false;
	try {
		for (const platform of platforms) {
			fs.rmSync(serverBinDir, { recursive: true, force: true });
			copyDir(path.join(holding, platform), path.join(serverBinDir, platform));
			console.log(`packaging ${VSIX_TARGETS[platform]} (${platform})`);
			vsixes.push(...packageOne(platform));
		}
		// Restore every binary for the universal vsix. It also leaves the staged
		// extension complete for the Open VSX step.
		fs.rmSync(serverBinDir, { recursive: true, force: true });
		copyDir(holding, serverBinDir);
		restored = true;
		console.log("packaging the universal fallback vsix");
		vsixes.push(...packageOne(undefined));
	} finally {
		// Key the restore on whether the loop finished, not on directory
		// existence: a mid-loop vsce failure leaves the failing platform's
		// binary in serverBinDir, which would skip the restore and let the
		// holding dir (the only complete copy) get deleted below.
		if (!restored) {
			fs.rmSync(serverBinDir, { recursive: true, force: true });
			copyDir(holding, serverBinDir);
		}
		fs.rmSync(holding, { recursive: true, force: true });
	}
	return vsixes;
}

function packageAllVsixes(): string[] {
	const platforms = stagedPlatforms();
	if (platforms.length === 0) {
		console.log(
			"no per-platform server binaries staged; packaging a single vsix",
		);
		return packageVsix();
	}
	const holding = path.join(artifactsRoot, "package", "server-staging");
	return runPlatformPackaging(serverBinDir, holding, platforms, (platform) =>
		platform === undefined ? packageVsix() : packageVsix(VSIX_TARGETS[platform]),
	);
}

// --- Release ---------------------------------------------------------------

interface ReleaseVersion {
	version: string;
	tag: string;
	preRelease: boolean;
}

// On a tag push CI sets TAG_RELEASE=true and the version comes from the tag.
// Manual/local runs fall back to the top CHANGELOG.md entry, which the heading
// regex reports without the v. Re-add it: the Release workflow only triggers on
// v*, and every published tag in the repo carries the prefix.
//
// Split from resolveVersion() so the derivation is unit-testable without
// touching process.env or the real CHANGELOG.
export function resolveVersionFrom(
	env: NodeJS.ProcessEnv,
	changelog: string,
): ReleaseVersion {
	const isTagRelease = /^(1|true)$/i.test(env.TAG_RELEASE ?? "");
	let tag = isTagRelease ? (env.GITHUB_REF_NAME ?? "").trim() : "";
	if (!tag) tag = `v${topChangelogVersion(changelog)}`;
	const version = tag.replace(/^v/, "");
	return { version, tag, preRelease: version.includes("-") };
}

function resolveVersion(): ReleaseVersion {
	return resolveVersionFrom(process.env, readChangelog());
}

function readChangelog(): string {
	return fs.readFileSync(path.join(repoRoot, "CHANGELOG.md"), "utf8");
}

function setReleaseVersion(version: string): void {
	const manifestPath = path.join(extensionDir, "package.json");
	let manifest: { version: string };
	try {
		manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8")) as {
			version: string;
		};
	} catch (error: unknown) {
		const message =
			error instanceof Error
				? error.message
				: typeof error === "string"
					? error
					: "unknown error";
		throw new Error(`could not parse ${manifestPath}: ${message}`, {
			cause: error,
		});
	}
	manifest.version = version;
	fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
	console.log(`set dist/extension/package.json version to ${version}`);
}

// Draft and publish the GitHub release with every vsix attached, via the gh
// CLI. If a release with the same tag already exists (e.g. from a previous
// failed run), delete it first so the workflow is idempotent. Throws before
// touching the release when the CHANGELOG has no section for this version.
function publishGithubRelease(
	tag: string,
	version: string,
	preRelease: boolean,
	vsixes: string[],
): void {
	const notes = releaseNotes(readChangelog(), version);
	const notesFile = path.join(vsixDir, "release-notes.md");
	fs.writeFileSync(notesFile, notes);

	// If the release already exists (e.g. retried workflow), remove it first.
	if (runOrNull("gh", ["release", "view", tag]) === 0) {
		console.log(`release ${tag} already exists; deleting before recreate`);
		run("gh", ["release", "delete", tag, "--yes"]);
	}

	const args = [
		"release",
		"create",
		tag,
		...vsixes,
		"--title",
		tag,
		"--notes-file",
		notesFile,
	];
	if (preRelease) args.push("--prerelease");
	run("gh", args);
}

function publishToMarketplace(vsixes: string[]): void {
	const token = process.env.VSCE_TOKEN;
	if (!token || !token.trim()) {
		const isTagRelease = /^(1|true)$/i.test(process.env.TAG_RELEASE ?? "");
		// A real tagged release must not silently degrade to GitHub-only — that
		// is exactly how Marketplace publishing went unnoticed. Only skip on
		// non-tag CI runs (PR/dispatch dry runs).
		if (process.env.CI && !isTagRelease) {
			console.log(
				"No VSCE_TOKEN set; skipping VS Code Marketplace publish (not a tag release).",
			);
			return;
		}
		throw new Error(
			"VSCE_TOKEN is not set; cannot publish to the Marketplace.",
		);
	}
	// vsce takes every platform-specific vsix in one publish, so the Marketplace
	// gets a consistent set rather than one platform at a time.
	run("npx", [
		"--no-install",
		"vsce",
		"publish",
		"--pat",
		token,
		"--packagePath",
		...vsixes,
	]);
}

// --- Commands --------------------------------------------------------------

function cmdCompile(): void {
	assembleClient();
}

function cmdQuick(): void {
	cleanExtensionDist();
	assembleClient();
	buildAndDeployRustServer();
}

function cmdPackage(): void {
	cleanExtensionDist();
	assembleClient();
	buildAndDeployRustServer();
	setReleaseVersion(resolveVersion().version);
	packageVsix();
}

// The vsixes packaged into artifacts/vsix by a previous package-prebuilt run,
// so CI can smoke-test them before anything is published.
function findVsixes(): string[] {
	const files = fs.existsSync(vsixDir)
		? fs.readdirSync(vsixDir).filter((f) => f.endsWith(".vsix"))
		: [];
	if (files.length === 0) {
		throw new Error(
			"no .vsix found in artifacts/vsix; run package-prebuilt first",
		);
	}
	return files.map((f) => path.join(vsixDir, f));
}

function cmdPackagePrebuilt(): string[] {
	// CI assembles the extension before staging the per-platform server binaries.
	// Do not clean or rebuild here, or those staged binaries are lost.
	setReleaseVersion(resolveVersion().version);
	return packageAllVsixes();
}

function cmdPublishPrebuilt(): void {
	const { version, tag, preRelease } = resolveVersion();
	const vsixes = findVsixes();
	publishGithubRelease(tag, version, preRelease, vsixes);
	publishToMarketplace(vsixes);
}

function cmdReleasePrebuilt(): void {
	cmdPackagePrebuilt();
	cmdPublishPrebuilt();
}

// Tag and push, nothing else: the v* push triggers the Release workflow, which
// builds the server on every platform, smoke-tests each vsix, and publishes.
// Packaging locally instead would ship whatever single binary the dev machine
// happens to have, untested.
function cmdRelease(): void {
	const { version, tag } = resolveVersion();
	// The notes guard sits in publishGithubRelease, which the workflow only
	// reaches after the matrix build. Check it here so a missing CHANGELOG
	// section can't leave a pushed tag behind.
	releaseNotes(readChangelog(), version);
	// Untracked files are fine; tracked edits are not.
	const dirty = runOrNull("git", ["diff", "--quiet", "HEAD"]);
	if (dirty !== 0) {
		throw new Error(
			"working tree has uncommitted changes; commit them before tagging a release",
		);
	}
	if (
		runOrNull("git", ["rev-parse", "--verify", "--quiet", `refs/tags/${tag}`]) === 0
	) {
		throw new Error(`tag ${tag} already exists locally`);
	}
	if (
		runOrNull("git", ["ls-remote", "--exit-code", "origin", `refs/tags/${tag}`]) === 0
	) {
		throw new Error(`tag ${tag} already exists on origin`);
	}
	run("git", ["tag", tag]);
	run("git", ["push", "origin", tag]);
	console.log(
		`pushed ${tag}; the Release workflow now builds, smoke-tests, and publishes it.`,
	);
}

const commands: Record<string, () => unknown> = {
	compile: cmdCompile,
	quick: cmdQuick,
	package: cmdPackage,
	"package-prebuilt": cmdPackagePrebuilt,
	"publish-prebuilt": cmdPublishPrebuilt,
	"release-prebuilt": cmdReleasePrebuilt,
	release: cmdRelease,
};

// Only dispatch when run directly (tsx build/build.ts <cmd>), not when the
// module is imported by the vitest suite to test runPlatformPackaging.
if (pathToFileURL(process.argv[1] ?? "").href === import.meta.url) {
	const cmd = process.argv[2] ?? "quick";
	const handler = commands[cmd];
	if (!handler) {
		console.error(
			`unknown command '${cmd}'. Known: ${Object.keys(commands).join(", ")}`,
		);
		process.exit(1);
	}
	handler();
}
