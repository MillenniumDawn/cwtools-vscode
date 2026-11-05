import * as vscode from "vscode";
import { errorMessage, logError } from "./logger";

export function guardCodeActionEdit(action: vscode.CodeAction): void {
	const data: unknown = (action as { data?: unknown }).data;
	if (!action.edit || !data || typeof data !== "object" || !("cwtoolsDocumentVersions" in data)) return;
	// vscode-languageclient drops TextDocumentEdit versions during conversion.
	action.command = {
		title: action.title,
		command: "cwtools.applyCodeAction",
		arguments: [action.edit, data.cwtoolsDocumentVersions],
	};
	action.edit = undefined;
}

export async function applyCodeAction(edit: unknown, versions: unknown): Promise<boolean> {
	try {
		if (!(edit instanceof vscode.WorkspaceEdit) || !Array.isArray(versions)) {
			throw new Error(vscode.l10n.t("The code action edit is invalid."));
		}
		for (const item of versions as unknown[]) {
			if (!item || typeof item !== "object" || !("uri" in item) || typeof item.uri !== "string" || !("version" in item)
				|| (item.version !== null && (typeof item.version !== "number" || !Number.isInteger(item.version)))) {
				throw new Error(vscode.l10n.t("The code action target version is invalid."));
			}
			if (item.version !== null && vscode.workspace.textDocuments.find((doc) => doc.uri.toString() === item.uri)?.version !== item.version) {
				throw new Error(vscode.l10n.t("A target document changed. Request the quick fix again."));
			}
		}
		if (!await vscode.workspace.applyEdit(edit)) {
			throw new Error(vscode.l10n.t("The code action edit could not be applied."));
		}
		return true;
	} catch (error) {
		logError("Failed to apply code action", error);
		void vscode.window.showErrorMessage(vscode.l10n.t("CWTools: could not apply quick fix: {0}", errorMessage(error)));
		return false;
	}
}
