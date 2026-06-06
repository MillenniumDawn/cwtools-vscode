/**
 * Minimal LSP-over-stdio client. Speaks just enough of the protocol to drive
 * a cwtools server binary (Rust or F#) directly, with no VS Code host. Used by
 * the host-free engine parity harness.
 */
import type { ChildProcessWithoutNullStreams} from 'child_process';
import { spawn } from 'child_process';

type Pending = (msg: { id: number; result?: unknown; error?: unknown }) => void;
export type NotificationHandler = (method: string, params: unknown) => void;

export class LspClient {
	private proc: ChildProcessWithoutNullStreams;
	private buf = Buffer.alloc(0);
	private nextId = 1;
	private readonly pending = new Map<number, Pending>();
	private readonly handlers: NotificationHandler[] = [];
	readonly stderr: string[] = [];

	constructor(command: string, args: string[] = []) {
		this.proc = spawn(command, args, { stdio: ['pipe', 'pipe', 'pipe'] });
		this.proc.stdout.on('data', chunk => this.onData(chunk as Buffer));
		this.proc.stderr.on('data', d => this.stderr.push(d.toString()));
	}

	onNotification(handler: NotificationHandler): void {
		this.handlers.push(handler);
	}

	request<T = unknown>(method: string, params: unknown): Promise<T> {
		const id = this.nextId++;
		return new Promise<T>((resolve, reject) => {
			this.pending.set(id, msg => {
				if (msg.error) reject(new Error(`${method} failed: ${JSON.stringify(msg.error)}`));
				else resolve(msg.result as T);
			});
			this.send({ jsonrpc: '2.0', id, method, params });
		});
	}

	notify(method: string, params: unknown): void {
		this.send({ jsonrpc: '2.0', method, params });
	}

	dispose(): void {
		try { this.proc.kill(); } catch { /* already gone */ }
	}

	private send(obj: unknown): void {
		const s = JSON.stringify(obj);
		this.proc.stdin.write(`Content-Length: ${Buffer.byteLength(s)}\r\n\r\n${s}`);
	}

	private onData(chunk: Buffer): void {
		this.buf = Buffer.concat([this.buf, chunk]);
		for (;;) {
			const headerEnd = this.buf.indexOf('\r\n\r\n');
			if (headerEnd < 0) return;
			const header = this.buf.slice(0, headerEnd).toString();
			const m = /Content-Length: (\d+)/i.exec(header);
			if (!m) { this.buf = this.buf.slice(headerEnd + 4); continue; }
			const len = parseInt(m[1], 10);
			const start = headerEnd + 4;
			if (this.buf.length < start + len) return;
			const body = this.buf.slice(start, start + len).toString();
			this.buf = this.buf.slice(start + len);
			let msg: { id?: number; method?: string; params?: unknown; result?: unknown; error?: unknown };
			try { msg = JSON.parse(body); } catch { continue; }
			if (msg.id !== undefined && this.pending.has(msg.id)) {
				const cb = this.pending.get(msg.id)!;
				this.pending.delete(msg.id);
				cb(msg as { id: number; result?: unknown; error?: unknown });
			} else if (msg.method) {
				for (const h of this.handlers) h(msg.method, msg.params);
			}
		}
	}
}
