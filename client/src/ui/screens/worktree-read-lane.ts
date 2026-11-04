
import { tr } from "../../i18n";/** One owned catalog read plus the newest waiting context. Obsolete promises
 * still settle; changing a folder never frees a running child's slot early. */
export class WorktreeReadLane<T = Record<string, unknown>> {
  private busy = false;
  private closed = false;
  private pending?: { run(): Promise<T>; resolve(value: T): void; reject(error: unknown): void };
  read(run: () => Promise<T>): Promise<T> {
    if (this.closed) return Promise.reject(tr("Worktree panel closed."));
    return new Promise((resolve, reject) => {
      this.pending?.reject(tr("Folder changed."));
      this.pending = { run, resolve, reject };
      this.pump();
    });
  }
  close() { this.closed = true; this.pending?.reject(tr("Worktree panel closed.")); this.pending = undefined; }
  private pump() {
    if (this.closed || this.busy || !this.pending) return;
    const job = this.pending; this.pending = undefined; this.busy = true;
    void Promise.resolve().then(job.run).then(job.resolve, job.reject).finally(() => { this.busy = false; this.pump(); });
  }
}
