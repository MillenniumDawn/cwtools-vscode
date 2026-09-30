import { suite, test } from "vitest";
import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";

const repoRoot = path.resolve(__dirname, "../../..");
const rootPackage = JSON.parse(
	fs.readFileSync(path.join(repoRoot, "package.json"), "utf8"),
) as {
	dependencies: Record<string, string>;
	devDependencies: Record<string, string>;
};
const dependencyReviewWorkflow = fs.readFileSync(
	path.join(repoRoot, ".github", "workflows", "ci.yml"),
	"utf8",
);

suite("runtime dependency classification (#572)", () => {
	test("shipped graph libraries are production dependencies", () => {
		for (const library of [
			"cytoscape",
			"cytoscape-elk",
			"cytoscape-popper",
			"tippy.js",
			"merge-images",
		]) {
			assert.ok(
				rootPackage.dependencies[library],
				`${library} must be in dependencies`,
			);
			assert.ok(
				!(library in rootPackage.devDependencies),
				`${library} must not remain in devDependencies`,
			);
		}
	});

	test("dependency review blocks vulnerable runtime and development scopes", () => {
		const start = dependencyReviewWorkflow.indexOf("  dependency-review:");
		const end = dependencyReviewWorkflow.indexOf("  # Full client build", start);
		assert.ok(start >= 0 && end > start, "dependency-review job must exist");
		const job = dependencyReviewWorkflow.slice(start, end);
		assert.ok(
			job.includes("actions/dependency-review-action@"),
			"dependency-review action must remain configured in its job",
		);
		assert.match(
			job,
			/^\s+fail-on-scopes:\s*runtime,\s*development\s*$/m,
		);
	});
});
