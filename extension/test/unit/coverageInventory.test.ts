import * as assert from "assert";
import { spawnSync } from "child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "fs";
import * as os from "os";
import * as path from "path";
import { test } from "vitest";

function inventory(include: string) {
	const root = mkdtempSync(path.join(os.tmpdir(), "cwtools-coverage-inventory-"));
	try {
		const source = path.join(root, "extension", "src", "host");
		mkdirSync(source, { recursive: true });
		writeFileSync(path.join(source, "value.ts"), "export const value = 1;");
		writeFileSync(path.join(source, "types.ts"), "export interface Shape { value: number }\nexport type Value = string;");
		writeFileSync(path.join(source, "importType.ts"), "import type { Shape } from './types'; export type Alias = Shape;");
		writeFileSync(path.join(source, "declared.d.ts"), "export declare const value: number;");
		writeFileSync(path.join(source, "effect.ts"), "console.log('loaded');");
		writeFileSync(path.join(root, "vitest.config.mts"),
			`export default defineConfig({ test: { include: ['wrong'], coverage: { include: ${include} } }, other: { include: ['also-wrong'] } });`);
		return spawnSync(process.execPath, [
			path.resolve("scripts/build/coverage_inventory.mjs"), root,
		], { encoding: "utf8" });
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
}

test("coverage inventory uses executable output and only test.coverage.include", () => {
	const result = inventory("['extension/src/host/value.ts']");
	assert.strictEqual(result.status, 0, result.stderr);
	const parsed = JSON.parse(result.stdout) as { sourceFiles: string[]; include: string[] };
	assert.deepStrictEqual(parsed.sourceFiles.sort(), [
		"extension/src/host/effect.ts", "extension/src/host/value.ts",
	]);
	assert.deepStrictEqual(parsed.include, ["extension/src/host/value.ts"]);
});

test.each(["[variable]", "[...files]", "files"])("coverage inventory fails closed for unsupported includes: %s", (include) => {
	const result = inventory(include);
	assert.notStrictEqual(result.status, 0);
	assert.match(result.stderr, /test\.coverage\.include/);
});
