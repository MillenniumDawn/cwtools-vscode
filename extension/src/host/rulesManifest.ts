import { Buffer } from "node:buffer";
import {
	LANGUAGE_REPOS,
	RULES_MANIFEST_REVISION,
	type RulesRepo,
} from "./games";

// Overridable so the rules-sync host suite can force this to fail fast and
// stay off the network, instead of racing a real manifest fetch against its
// own local fixture (see extension/test/host/rulesSync.test.ts). Unset in
// every real install.
export const RULES_MANIFEST_URL =
	process.env.CWTOOLS_TEST_RULES_MANIFEST_URL ||
	"https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/main/rules-pins.json";
export const RULES_MANIFEST_CACHE_KEY = "rulesManifest";
export const RULES_MANIFEST_MAX_BYTES = 64 * 1024;
export const RULES_MANIFEST_TIMEOUT_MS = 10_000;

export interface RulesManifest {
	schema: 1;
	revision: number;
	pins: Record<string, string>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function sameStrings(left: string[], right: string[]): boolean {
	return (
		left.length === right.length && left.every((value, i) => value === right[i])
	);
}

function sorted(values: string[]): string[] {
	return [...values].sort((left, right) => left.localeCompare(right));
}

function samePins(left: RulesManifest, right: RulesManifest): boolean {
	return Object.entries(left.pins).every(([id, ref]) => right.pins[id] === ref);
}

// The manifest is data only: it can move known refs, never add a repo or name
// a branch. The bundled table remains the bootstrap and repo-url allowlist.
export function parseRulesManifest(value: unknown): RulesManifest {
	if (!isRecord(value)) {
		throw new Error("Rules manifest must be an object.");
	}
	const keys = sorted(Object.keys(value));
	if (!sameStrings(keys, ["pins", "revision", "schema"])) {
		throw new Error("Rules manifest has unexpected fields.");
	}
	if (value.schema !== 1) {
		throw new Error("Rules manifest has an unsupported schema.");
	}
	if (
		typeof value.revision !== "number" ||
		!Number.isSafeInteger(value.revision) ||
		value.revision < RULES_MANIFEST_REVISION
	) {
		throw new Error("Rules manifest has an invalid revision.");
	}
	if (!isRecord(value.pins)) {
		throw new Error("Rules manifest pins must be an object.");
	}

	const ids = Object.keys(LANGUAGE_REPOS);
	const pinIds = sorted(Object.keys(value.pins));
	if (!sameStrings(pinIds, sorted(ids))) {
		throw new Error("Rules manifest pins do not match the supported games.");
	}

	const pins: Record<string, string> = {};
	for (const id of ids) {
		const ref = value.pins[id];
		if (typeof ref !== "string" || !/^[0-9a-f]{40}$/.test(ref)) {
			throw new Error(`Rules manifest has an invalid ${id} ref.`);
		}
		pins[id] = ref;
	}
	return { schema: 1, revision: value.revision, pins };
}

export function parseRulesManifestText(text: string): RulesManifest {
	if (Buffer.byteLength(text, "utf8") > RULES_MANIFEST_MAX_BYTES) {
		throw new Error("Rules manifest is too large.");
	}
	try {
		return parseRulesManifest(JSON.parse(text) as unknown);
	} catch (err) {
		if (err instanceof SyntaxError) {
			throw Object.assign(new Error("Rules manifest is not valid JSON."), {
				cause: err,
			});
		}
		throw err;
	}
}

export async function readRulesManifestBody(
	body: ReadableStream<Uint8Array> | null,
): Promise<string> {
	const reader = body?.getReader();
	if (!reader) {
		throw new Error("Rules manifest response has no body.");
	}
	const decoder = new TextDecoder();
	let bytes = 0;
	let text = "";
	try {
		while (true) {
			const { done, value } = await reader.read();
			if (done) {
				return text + decoder.decode();
			}
			bytes += value.byteLength;
			if (bytes > RULES_MANIFEST_MAX_BYTES) {
				await reader.cancel().catch(() => undefined);
				throw new Error("Rules manifest response is too large.");
			}
			text += decoder.decode(value, { stream: true });
		}
	} finally {
		reader.releaseLock();
	}
}

// A revision can deliberately roll a pin back, but it must still advance. This
// prevents a stale response or an older cache from moving rules backwards.
export function selectRulesManifest(
	cached: RulesManifest | undefined,
	fetched: RulesManifest,
): RulesManifest {
	if (!cached || fetched.revision > cached.revision) {
		return fetched;
	}
	if (fetched.revision < cached.revision) {
		return cached;
	}
	if (!samePins(cached, fetched)) {
		throw new Error("Rules manifest revision conflicts with the cached pins.");
	}
	return cached;
}

export function rulesRepoForManifest(
	language: string,
	manifest: RulesManifest | undefined,
): RulesRepo | undefined {
	const bundled = LANGUAGE_REPOS[language];
	if (!bundled || !manifest) {
		return bundled;
	}
	const ref = manifest.pins[language];
	return ref ? { ...bundled, ref } : bundled;
}
