import { window, commands, env, Uri, l10n } from "vscode";

// EPERM/EACCES means the OS refused to execute the server binary, almost
// always antivirus (Defender) quarantining the unsigned exe or a corporate
// exec policy. A raw "spawn EPERM" tells a modder nothing, so surface the
// cause and a self-serve fix instead. Shared by the activation start path and
// the restart command, which both spawn the same binary the same way.
export function showServerBlockedDialog(
	err: unknown,
	serverExe: string | undefined,
): boolean {
	const code = (err as NodeJS.ErrnoException | undefined)?.code;
	if (code !== "EPERM" && code !== "EACCES") {
		return false;
	}
	const reveal = l10n.t("Reveal Server Binary");
	const help = l10n.t("Antivirus Help");
	void window
		.showErrorMessage(
			l10n.t(
				"CWTools server was blocked from running ({0}). This is almost always antivirus (e.g. Windows Defender) quarantining the unsigned server binary. Restore it from quarantine and add an exclusion for the extension's server folder, then reload the window.",
				code,
			),
			reveal,
			help,
		)
		.then((choice) => {
			if (choice === reveal && serverExe) {
				void commands.executeCommand("revealFileInOS", Uri.file(serverExe));
			} else if (choice === help) {
				void env.openExternal(
					Uri.parse(
						"https://support.microsoft.com/windows/add-an-exclusion-to-windows-security-811816c0-4dfd-af4a-47e4-c301afe13b26",
					),
				);
			}
		});
	return true;
}
