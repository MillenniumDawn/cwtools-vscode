import { readFileSync, readdirSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import ts from "typescript";

const root = resolve(process.argv[2]);
const sourceRoot = join(root, "extension", "src");
const sourceFiles = [];

function visit(directory) {
	for (const entry of readdirSync(directory, { withFileTypes: true })) {
		const path = join(directory, entry.name);
		if (entry.isDirectory()) visit(path);
		else if (entry.isFile() && entry.name.endsWith(".ts") && !entry.name.endsWith(".d.ts")) {
			const output = ts.transpileModule(readFileSync(path, "utf8"), {
				compilerOptions: { module: ts.ModuleKind.ESNext, removeComments: true },
			}).outputText.trim().replace(/^export\s*\{\s*\};?$/gm, "").trim();
			if (output) sourceFiles.push(relative(root, path).split(sep).join("/"));
		}
	}
}

function property(object, name) {
	return object.properties.find(
		(item) => ts.isPropertyAssignment(item) &&
			((ts.isIdentifier(item.name) && item.name.text === name) ||
			 (ts.isStringLiteral(item.name) && item.name.text === name)),
	);
}

visit(sourceRoot);
const config = ts.createSourceFile(
	"vitest.config.mts",
	readFileSync(join(root, "vitest.config.mts"), "utf8"),
	ts.ScriptTarget.Latest,
	true,
);
let rootConfig;
function findConfig(node) {
	if (
		ts.isCallExpression(node) &&
		ts.isIdentifier(node.expression) &&
		node.expression.text === "defineConfig" &&
		node.arguments.length > 0 &&
		ts.isObjectLiteralExpression(node.arguments[0])
	) rootConfig = node.arguments[0];
	ts.forEachChild(node, findConfig);
}
findConfig(config);
const test = rootConfig && property(rootConfig, "test")?.initializer;
const coverage = test && ts.isObjectLiteralExpression(test)
	? property(test, "coverage")?.initializer
	: undefined;
const includeProperty = coverage && ts.isObjectLiteralExpression(coverage)
	? property(coverage, "include")
	: undefined;
if (!includeProperty || !ts.isArrayLiteralExpression(includeProperty.initializer)) {
	throw new Error("could not find test.coverage.include array in vitest.config.mts");
}
const include = includeProperty.initializer.elements.map((element) => {
	if (!ts.isStringLiteral(element)) {
		throw new Error("test.coverage.include entries must be string literals");
	}
	return element.text;
});
process.stdout.write(JSON.stringify({ sourceFiles, include }));
