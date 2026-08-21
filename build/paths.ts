import * as path from "node:path";
import { fileURLToPath } from "node:url";

export const repoRoot = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	"..",
);

export const engineRoot = path.join(repoRoot, "engine");
export const extensionRoot = path.join(repoRoot, "extension");
export const extensionSourceRoot = path.join(extensionRoot, "src");
export const extensionHostRoot = path.join(extensionSourceRoot, "host");
export const extensionWebviewRoot = path.join(extensionSourceRoot, "webview");
export const extensionTestRoot = path.join(extensionRoot, "test");
export const extensionPackageRoot = path.join(extensionRoot, "package");
export const extensionDocsRoot = path.join(repoRoot, "docs", "extension");
export const extensionDistRoot = path.join(repoRoot, "dist", "extension");
export const artifactsRoot = path.join(repoRoot, "artifacts");
export const vsixRoot = path.join(artifactsRoot, "vsix");
