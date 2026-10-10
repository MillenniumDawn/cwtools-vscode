import { suite, test } from "vitest";
import * as assert from "assert";
import * as path from "path";
import { gameContentRoots } from "../../src/host/gameContentRoots";

suite("game content roots", () => {
	test("includes selected, relative parent and configured vanilla roots", () => {
		const root = path.resolve("fixtures", "submod");
		const parent = path.resolve(root, "..", "parent");
		const vanilla = path.resolve("fixtures", "vanilla");
		assert.deepStrictEqual(gameContentRoots(root, ["../parent"], vanilla), [root, parent, vanilla]);
	});

	test("rejects parents inside, equal to or enclosing the selected root", () => {
		const root = path.resolve("fixtures", "submod");
		const sibling = `${root}-other`;
		assert.deepStrictEqual(gameContentRoots(root, [root, ".", "..", "nested", sibling]), [root, sibling]);
	});
});
