/** Publish the identity snapshot, with a revocable lifetime for enforcing gates. */
import { closeSync, fstatSync, openSync, readFileSync, renameSync, statSync, writeFileSync } from 'node:fs';

export function finalRecords(root, name, enforce) {
  const path = `${root}/${name}-final-records`;
  let current;
  let closed = false;
  const revoke = () => {
    if (current === undefined) return;
    const fd = current;
    current = undefined;
    closeSync(fd);
  };
  return {
    close() { closed = true; revoke(); },
    publish(records) {
      if (closed) throw Error('AW final identity snapshot is closed');
      const temporary = `${path}.tmp`;
      if (!enforce) {
        try {
          writeFileSync(temporary, JSON.stringify(records), { mode: 0o600 });
          renameSync(temporary, `${path}.json`);
          return true;
        } catch {
          return false;
        }
      }
      // Revocation uses a process-owned descriptor and succeeds even if the
      // filesystem cannot replace or remove the previous snapshot.
      revoke();
      const fd = openSync(temporary, 'w', 0o600);
      try {
        writeFileSync(fd, JSON.stringify({ pid: process.pid, fd, records }));
        renameSync(temporary, `${path}.json`);
        current = fd;
        return true;
      } finally {
        if (current !== fd) closeSync(fd);
      }
    },
  };
}

export function readFinalRecords(root, name, enforce) {
  const path = `${root}/${name}-final-records.json`;
  if (!enforce) return JSON.parse(readFileSync(path, 'utf8'));
  let snapshot;
  try {
    snapshot = openSync(path, 'r');
    const { pid, fd, records } = JSON.parse(readFileSync(snapshot, 'utf8'));
    if (!Number.isInteger(pid) || pid <= 0 || !Number.isInteger(fd) || fd < 0 ||
        (pid === process.pid && fd === snapshot) ||
        !records || typeof records !== 'object' || Array.isArray(records)) return {};
    const actual = fstatSync(snapshot, { bigint: true });
    const live = statSync(`/proc/${pid}/fd/${fd}`, { bigint: true });
    // Descriptor numbers can be reused. Match the inode held by the writer
    // to the file we actually read, even if the pathname has since changed.
    return actual.ino === live.ino && actual.dev === live.dev ? records : {};
  } catch {
    return {};
  } finally {
    if (snapshot !== undefined) closeSync(snapshot);
  }
}
