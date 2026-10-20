import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createInterface } from "node:readline";

export type BridgeResponse = { id: number; ok: boolean; error?: string; details?: string[]; [k: string]: unknown };

/** Long-lived Python renderer process, spoken to over JSON lines. */
export class RendererBridge {
  private proc: ChildProcessWithoutNullStreams | null = null;
  private nextId = 1;
  private pending = new Map<number, { resolve: (r: BridgeResponse) => void; reject: (e: Error) => void }>();

  constructor(private readonly factoryDir: string, private readonly python = process.env.SPRITE_PYTHON ?? "python3") {}

  private start(): ChildProcessWithoutNullStreams {
    if (this.proc) return this.proc;
    const proc = spawn(this.python, ["bridge.py"], { cwd: this.factoryDir, stdio: ["pipe", "pipe", "pipe"] });
    createInterface({ input: proc.stdout }).on("line", (line) => {
      let msg: BridgeResponse;
      try {
        msg = JSON.parse(line);
      } catch {
        return;
      }
      const p = this.pending.get(msg.id);
      if (p) {
        this.pending.delete(msg.id);
        p.resolve(msg);
      }
    });
    proc.stderr.on("data", (d) => process.stderr.write(`[renderer] ${d}`));
    proc.on("exit", (code) => {
      for (const p of this.pending.values()) p.reject(new Error(`renderer exited (${code})`));
      this.pending.clear();
      this.proc = null;
    });
    this.proc = proc;
    return proc;
  }

  call(cmd: string, payload: Record<string, unknown> = {}, timeoutMs = 120_000): Promise<BridgeResponse> {
    const proc = this.start();
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`renderer timed out on ${cmd}`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (r) => (clearTimeout(timer), resolve(r)),
        reject: (e) => (clearTimeout(timer), reject(e)),
      });
      proc.stdin.write(JSON.stringify({ id, cmd, ...payload }) + "\n");
    });
  }

  close(): void {
    this.proc?.kill();
  }
}
