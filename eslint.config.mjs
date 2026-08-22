import eslint from "@eslint/js";
import tseslint from "typescript-eslint";
import globals from "globals";
import { includeIgnoreFile } from "@eslint/compat";
import { fileURLToPath } from "node:url";

const gitignorePath = fileURLToPath(new URL(".gitignore", import.meta.url));

export default tseslint.config(
	eslint.configs.recommended,
	tseslint.configs.recommended,
	// Type-aware rules need the project; scope them (and projectService) to
	// TypeScript files only, so config/JS files aren't type-checked.
	{
		files: ["**/*.ts"],
		extends: [tseslint.configs.recommendedTypeChecked],
		languageOptions: {
			parserOptions: {
			projectService: {
				allowDefaultProject: ["vitest.config.ts"],
				maximumDefaultProjectFileMatchCount_THIS_WILL_SLOW_DOWN_LINTING:
					16,
			},
			},
		},
	},
	includeIgnoreFile(gitignorePath, "Imported .gitignore patterns"),
	// The Rust engine has no JS to lint, and its target/ is ignored by its own
	// .gitignore, which the root one above doesn't cover. Without this eslint
	// walks the whole build output.
	{ ignores: ["engine/**"] },
	// Extension host, build scripts, and configs run under Node.
	{
		languageOptions: { globals: globals.node },
		rules: {
			eqeqeq: "error",
			"@typescript-eslint/no-unused-vars": [
				"error",
				{
					argsIgnorePattern: "^_",
					varsIgnorePattern: "^_",
					caughtErrorsIgnorePattern: "^_",
				},
			],
			"@typescript-eslint/no-explicit-any": "error",
			"@typescript-eslint/consistent-type-imports": [
				"error",
				{ prefer: "type-imports" },
			],
		},
	},
	// The webview runs in a browser context.
	{
		files: ["extension/src/webview/**/*.ts"],
		languageOptions: { globals: globals.browser },
	},
	// chai assertions like `expect(x).to.be.true` read as unused expressions.
	{
		files: ["extension/test/**/*.ts"],
		rules: { "@typescript-eslint/no-unused-expressions": "off" },
	},
);
