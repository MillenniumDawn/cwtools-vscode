// Build orchestrator for the extension. Replaces the old FAKE/dotnet script.
// Run via `tsx build/build.ts <command>` (see package.json scripts and build.sh).
//
// Commands:
//   quick            Local dev build: Rust server + client into release/, ready to launch.
//   package          Clean, build the client, package a vsix into temp/ (no server build).
//   package-prebuilt Package the staged per-platform binaries into vsixes without publishing.
//   publish-prebuilt Publish the vsixes already packaged into temp/. CI packages, smoke-tests,
//                    then publishes, so a broken package never reaches the Marketplace.
//   release-prebuilt Set the version, package the staged binaries into one vsix per platform
//                    plus a universal fallback, draft the GitHub release, and publish to the
//                    Marketplace. Used by CI after the per-platform Rust binaries are staged
//                    under release/bin/server.
//   release          Tag the current CHANGELOG version, push, then run release-prebuilt.
//
// The Rust server (cwtools-rs) builds from a sibling checkout by default; set
// CWTOOLS_RUST_WORKSPACE to build from elsewhere (e.g. the submodule).

import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { releaseNotes, topChangelogVersion } from "./changelog";

const repoRoot = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	"..",
);
const releaseDir = path.join(repoRoot, "release");
const tempDir = path.join(repoRoot, "temp");

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
	return path.resolve(repoRoot, "../cwtools/cwtools-rs");
}

function buildAndDeployRustServer(): void {
	const workspace = rustWorkspace();
	run("cargo", ["build", "--release", "-p", "cwtools_lsp"], { cwd: workspace });

	const binName = isWindows ? "cwtools-server.exe" : "cwtools-server";
	const built = path.join(workspace, "target/release", binName);
	if (!fs.existsSync(built)) {
		throw new Error(
			`Rust server binary not found at '${built}' after build. Check the crate name/target, ` +
				`or point CWTOOLS_RUST_WORKSPACE at the right cwtools-rs checkout (currently '${workspace}').`,
		);
	}

	// Deploy to the path the client loads first. Clean it so stale binaries
	// don't linger next to the fresh one.
	const outDir = path.join(releaseDir, "bin/server/cwtools-server");
	fs.rmSync(outDir, { recursive: true, force: true });
	fs.mkdirSync(outDir, { recursive: true });
	const dest = path.join(outDir, binName);
	fs.copyFileSync(built, dest);
	if (!isWindows) fs.chmodSync(dest, 0o755);
}

// --- Client + docs ---------------------------------------------------------

function buildClient(): void {
	run("npm", ["run", "compile"]);
}

function copyDocs(): void {
	for (const f of ["README.md", "LICENSE.md"]) {
		fs.copyFileSync(path.join(repoRoot, f), path.join(releaseDir, f));
	}
	fs.copyFileSync(
		path.join(repoRoot, "CHANGELOG.md"),
		path.join(releaseDir, "CHANGELOG.md"),
	);
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
	const dest = path.join(releaseDir, "bin/client/webview");
	fs.mkdirSync(dest, { recursive: true });
	const webviewSrc = path.join(repoRoot, "client/webview");
	for (const f of fs.readdirSync(webviewSrc)) {
		if (f.endsWith(".css"))
			fs.copyFileSync(path.join(webviewSrc, f), path.join(dest, f));
	}
}

function copyTestSamples(): void {
	copyDir(
		path.join(repoRoot, "client/test/sample"),
		path.join(releaseDir, "bin/client/test/sample"),
	);
}

function cleanReleaseBin(): void {
	fs.rmSync(path.join(releaseDir, "bin"), { recursive: true, force: true });
}

function assembleClient(): void {
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

const serverBinDir = path.join(releaseDir, "bin/server/cwtools-server");

function packageVsix(target?: string): string[] {
	// The client is bundled with esbuild, so node_modules is excluded from the
	// vsix (see release/.vscodeignore). --no-dependencies stops vsce from trying
	// to resolve/include them.
	const args = ["--yes", "@vscode/vsce", "package", "--no-dependencies"];
	if (target) args.push("--target", target);
	run("npx", args, { cwd: releaseDir });
	fs.mkdirSync(tempDir, { recursive: true });
	const packaged: string[] = [];
	for (const f of fs.readdirSync(releaseDir)) {
		if (f.endsWith(".vsix")) {
			const dest = path.join(tempDir, f);
			fs.renameSync(path.join(releaseDir, f), dest);
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
function packageAllVsixes(): string[] {
	const platforms = stagedPlatforms();
	if (platforms.length === 0) {
		console.log(
			"no per-platform server binaries staged; packaging a single vsix",
		);
		return packageVsix();
	}

	const holding = path.join(tempDir, "server-staging");
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
			vsixes.push(...packageVsix(VSIX_TARGETS[platform]));
		}
		// Restore every binary for the universal vsix. It also leaves release/
		// complete for the Open VSX step, which re-packages from the directory.
		fs.rmSync(serverBinDir, { recursive: true, force: true });
		copyDir(holding, serverBinDir);
		restored = true;
		console.log("packaging the universal fallback vsix");
		vsixes.push(...packageVsix());
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

// --- Release ---------------------------------------------------------------

// On a tag push CI sets TAG_RELEASE=true and the version comes from the tag.
// Manual/local runs fall back to the top CHANGELOG.md entry.
function resolveVersion(): {
	version: string;
	tag: string;
	preRelease: boolean;
} {
	const isTagRelease = /^(1|true)$/i.test(process.env.TAG_RELEASE ?? "");
	let tag = isTagRelease ? (process.env.GITHUB_REF_NAME ?? "").trim() : "";
	if (!tag) tag = topChangelogVersion(readChangelog());
	const version = tag.replace(/^v/, "");
	return { version, tag, preRelease: version.includes("-") };
}

function readChangelog(): string {
	return fs.readFileSync(path.join(repoRoot, "CHANGELOG.md"), "utf8");
}

function setReleaseVersion(version: string): void {
	const manifestPath = path.join(releaseDir, "package.json");
	let manifest: { version: string };
	try {
		manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8")) as {
			version: string;
		};
	} catch (e) {
		throw new Error(`could not parse ${manifestPath}`, { cause: e });
	}
	manifest.version = version;
	fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
	console.log(`set release/package.json version to ${version}`);
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
	const notesFile = path.join(tempDir, "release-notes.md");
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
		"--yes",
		"@vscode/vsce",
		"publish",
		"--pat",
		token,
		"--packagePath",
		...vsixes,
	]);
}

// --- Commands --------------------------------------------------------------

function cmdQuick(): void {
	cleanReleaseBin();
	buildAndDeployRustServer();
	assembleClient();
}

function cmdPackage(): void {
	cleanReleaseBin();
	setReleaseVersion(resolveVersion().version);
	assembleClient();
	packageVsix();
}

// The vsixes packaged into temp/ by a previous package-prebuilt run, so CI can
// smoke-test them before anything is published.
function findVsixes(): string[] {
	const files = fs.existsSync(tempDir)
		? fs.readdirSync(tempDir).filter((f) => f.endsWith(".vsix"))
		: [];
	if (files.length === 0)
		throw new Error("no .vsix found in temp/; run package-prebuilt first");
	return files.map((f) => path.join(tempDir, f));
}

function cmdPackagePrebuilt(): string[] {
	setReleaseVersion(resolveVersion().version);
	// Build the client bundles (extension.js, webview/graph.js) into release/bin
	// so vsce finds the entrypoint. No cleanReleaseBin here: the per-platform
	// server binaries are already staged under release/bin/server and assembling
	// the client doesn't touch them.
	assembleClient();
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

function cmdRelease(): void {
	const { version, tag } = resolveVersion();
	// The notes guard sits in publishGithubRelease, which is only reached after
	// the build. Check it here so a missing CHANGELOG section can't leave a
	// pushed tag behind.
	releaseNotes(readChangelog(), version);
	run("git", ["tag", tag]);
	run("git", ["push", "origin", tag]);
	cmdReleasePrebuilt();
}

const commands: Record<string, () => unknown> = {
	quick: cmdQuick,
	package: cmdPackage,
	"package-prebuilt": cmdPackagePrebuilt,
	"publish-prebuilt": cmdPublishPrebuilt,
	"release-prebuilt": cmdReleasePrebuilt,
	release: cmdRelease,
};

const cmd = process.argv[2] ?? "quick";
const handler = commands[cmd];
if (!handler) {
	console.error(
		`unknown command '${cmd}'. Known: ${Object.keys(commands).join(", ")}`,
	);
	process.exit(1);
}
handler();
