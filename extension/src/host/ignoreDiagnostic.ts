import * as fs from "node:fs/promises";
import { applyEdits, createScanner, findNodeAtLocation, getNodeValue, modify, parseTree, SyntaxKind, type ModificationOptions, type ParseError } from "jsonc-parser/lib/esm/main.js";
import * as vscode from "vscode";
import { errorMessage, logError } from "./logger";

function commentsIn(text: string): string[] {
	const scanner = createScanner(text);
	const comments: string[] = [];
	for (let token = scanner.scan(); token !== SyntaxKind.EOF; token = scanner.scan()) {
		if (token === SyntaxKind.LineCommentTrivia || token === SyntaxKind.BlockCommentTrivia) {
			comments.push(text.slice(scanner.getTokenOffset(), scanner.getTokenOffset() + scanner.getTokenLength()));
		}
	}
	return comments;
}

function modifySettings(text: string, path: (string | number)[], value: unknown, options: ModificationOptions): string {
	const edits = modify(text, path, value, options).map((edit) => {
		const kept = commentsIn(edit.content);
		const removed = commentsIn(text.slice(edit.offset, edit.offset + edit.length)).filter((comment) => {
			const index = kept.indexOf(comment);
			if (index < 0) return true;
			kept.splice(index, 1);
			return false;
		});
		return { ...edit, content: removed.length ? `${removed.join("\n")}\n${edit.content}` : edit.content };
	});
	return applyEdits(text, edits);
}

let pending: Promise<unknown> = Promise.resolve();

export function ignoreDiagnosticCode(uriValue: unknown, codeValue: unknown): Promise<boolean> {
	const next = pending.then(() => updateIgnoredCodes(uriValue, codeValue));
	pending = next;
	return next;
}

async function updateIgnoredCodes(uriValue: unknown, codeValue: unknown): Promise<boolean> {
	try {
		if (typeof uriValue !== "string" || typeof codeValue !== "string" || !/^CW\d{3}$/i.test(codeValue)) {
			throw new Error(vscode.l10n.t("The diagnostic code or document URI is invalid."));
		}
		const uri = vscode.Uri.parse(uriValue, true);
		const folder = vscode.workspace.getWorkspaceFolder(uri);
		if (uri.scheme !== "file" || !folder || folder.uri.toString() !== uri.toString()) {
			throw new Error(vscode.l10n.t("The ignore action does not name a local workspace folder."));
		}
		const settingsDir = vscode.Uri.joinPath(folder.uri, ".vscode");
		const settingsUri = vscode.Uri.joinPath(settingsDir, "settings.json");
		let exists = false;
		for (const [target, directory] of [[settingsDir, true], [settingsUri, false]] as const) {
			let stat;
			try {
				stat = await fs.lstat(target.fsPath);
			} catch (error) {
				if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
				throw error;
			}
			if (stat.isSymbolicLink() || (directory ? !stat.isDirectory() : !stat.isFile())) {
				throw new Error(vscode.l10n.t("The settings path is a symbolic link or has the wrong file type: {0}", target.fsPath));
			}
			if (!directory) exists = true;
		}
		const document = exists ? await vscode.workspace.openTextDocument(settingsUri) : undefined;
		// Submit synchronously from this snapshot so applyEdit captures its document version.
		const text = document?.getText() ?? "";
		const wasDirty = document?.isDirty ?? false;
		const errors: ParseError[] = [];
		const root = parseTree(text, errors, { allowTrailingComma: true, allowEmptyContent: true });
		if (errors.length || (root && root.type !== "object")) {
			throw new Error(vscode.l10n.t("Workspace settings must be a valid JSONC object."));
		}
		const namespace = root && findNodeAtLocation(root, ["cwtools"]);
		if (namespace && namespace.type !== "object") throw new Error(vscode.l10n.t("The nested cwtools settings must be an object."));
		const flat = root && findNodeAtLocation(root, ["cwtools.errors.ignore"]);
		const nested = root && findNodeAtLocation(root, ["cwtools", "errors", "ignore"]);
		const moveFlat = Boolean(flat && namespace && flat.offset < namespace.offset);
		const inspection = vscode.workspace.getConfiguration("cwtools", folder.uri).inspect<string[]>("errors.ignore");
		const inherited = (vscode.workspace.workspaceFile ? inspection?.workspaceValue : undefined) ?? inspection?.globalValue ?? [];
		const lists: unknown[] = [];
		for (const node of [flat, nested]) {
			if (node) lists.push(getNodeValue(node));
		}
		if (!lists.length) lists.push(inherited);
		const ignored: string[] = [];
		for (const list of lists) {
			if (!Array.isArray(list) || !list.every((code): code is string => typeof code === "string")) {
				throw new Error(vscode.l10n.t("cwtools.errors.ignore must be an array of diagnostic codes."));
			}
			for (const code of list) {
				if (!ignored.some((existing) => existing.toUpperCase() === code.toUpperCase())) ignored.push(code);
			}
		}
		const code = codeValue.toUpperCase();
		const present = ignored.some((existing) => existing.toUpperCase() === code);
		if (present && !nested && !moveFlat) return true;
		if (!present) ignored.push(code);
		const options = { formattingOptions: { insertSpaces: true, tabSize: 4, eol: document?.eol === vscode.EndOfLine.CRLF ? "\r\n" : "\n" } };
		let updated = nested ? modifySettings(text, ["cwtools", "errors", "ignore"], undefined, options) : text;
		if (moveFlat) updated = modifySettings(updated, ["cwtools.errors.ignore"], undefined, options);
		updated = flat && !nested && !moveFlat
			? modifySettings(updated, ["cwtools.errors.ignore", -1], code, options)
			: modifySettings(updated, ["cwtools.errors.ignore"], ignored, options);
		const edit = new vscode.WorkspaceEdit();
		if (document) {
			let start = 0;
			while (start < text.length && start < updated.length && text[start] === updated[start]) start++;
			let end = text.length;
			let updatedEnd = updated.length;
			while (end > start && updatedEnd > start && text[end - 1] === updated[updatedEnd - 1]) { end--; updatedEnd--; }
			edit.replace(settingsUri, new vscode.Range(document.positionAt(start), document.positionAt(end)), updated.slice(start, updatedEnd));
		} else {
			edit.createFile(settingsUri);
			edit.insert(settingsUri, new vscode.Position(0, 0), updated);
		}
		if (!await vscode.workspace.applyEdit(edit)) throw new Error(vscode.l10n.t("The settings edit could not be applied."));
		if (wasDirty) {
			void vscode.window.showInformationMessage(vscode.l10n.t("CWTools: added {0} to unsaved settings. Save the file to apply it.", code));
		} else {
			const editedDocument = document ?? await vscode.workspace.openTextDocument(settingsUri);
			if (!await editedDocument.save()) throw new Error(vscode.l10n.t("The settings edit could not be saved."));
		}
		return true;
	} catch (error) {
		logError("Failed to ignore diagnostic code", error);
		void vscode.window.showErrorMessage(vscode.l10n.t("CWTools: could not ignore diagnostic code: {0}", errorMessage(error)));
		return false;
	}
}
