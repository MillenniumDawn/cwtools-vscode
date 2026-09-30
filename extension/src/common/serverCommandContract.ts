import serverCommandIds from "./serverCommands.json";

export type ServerCommandName = Extract<keyof typeof serverCommandIds, string>;

export const SERVER_COMMAND_NAMES = Object.keys(
	serverCommandIds,
) as ServerCommandName[];

// Keep client-dispatched names constrained to the shared contract. The typed
// identity also lets callers reuse the server's exact protocol spelling.
export function serverCommand<Command extends ServerCommandName>(
	command: Command,
): Command {
	return command;
}

export function isServerCommand(command: string): command is ServerCommandName {
	return Object.prototype.hasOwnProperty.call(serverCommandIds, command);
}
