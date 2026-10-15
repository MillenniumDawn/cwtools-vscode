export const MAX_RESTARTS = 4;
const RESTART_WINDOW_MS = 3 * 60 * 1000;

// Records a crash and says whether the server may be restarted: the crash after
// MAX_RESTARTS inside the window gives up, an older one just ages out.
export function recordCrash(crashes: number[], now: number): boolean {
	crashes.push(now);
	if (crashes.length <= MAX_RESTARTS) {
		return true;
	}
	if (now - crashes[0] <= RESTART_WINDOW_MS) {
		return false;
	}
	crashes.shift();
	return true;
}
