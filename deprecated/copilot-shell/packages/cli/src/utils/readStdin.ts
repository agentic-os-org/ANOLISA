/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { StringDecoder } from 'node:string_decoder';

export async function readStdin(): Promise<string> {
  const MAX_STDIN_SIZE = 8 * 1024 * 1024; // 8MB
  return new Promise((resolve, reject) => {
    let data = '';
    let totalSize = 0;
    process.stdin.setEncoding('utf8');

    const pipedInputShouldBeAvailableInMs = 500;
    let pipedInputTimerId: null | NodeJS.Timeout = setTimeout(() => {
      // stop reading if input is not available yet, this is needed
      // in terminals where stdin is never TTY and nothing's piped
      // which causes the program to get stuck expecting data from stdin
      onEnd();
    }, pipedInputShouldBeAvailableInMs);

    const onReadable = () => {
      let chunk;
      while ((chunk = process.stdin.read()) !== null) {
        if (pipedInputTimerId) {
          clearTimeout(pipedInputTimerId);
          pipedInputTimerId = null;
        }

        const chunkSize = Buffer.byteLength(chunk, 'utf8');
        if (totalSize + chunkSize > MAX_STDIN_SIZE) {
          const remainingSize = MAX_STDIN_SIZE - totalSize;
          // A byte boundary can bisect a character; keep only its complete
          // UTF-8 prefix instead of adding a replacement or lone surrogate.
          data += new StringDecoder('utf8').write(
            Buffer.from(chunk, 'utf8').subarray(0, remainingSize),
          );
          console.warn(
            `Warning: stdin input truncated to ${MAX_STDIN_SIZE} bytes.`,
          );
          onEnd();
          process.stdin.destroy(); // A normal destroy emits close, not end.
          return;
        }
        data += chunk;
        totalSize += chunkSize;
      }
    };

    const onEnd = () => {
      cleanup();
      resolve(data);
    };

    const onError = (err: Error) => {
      cleanup();
      reject(err);
    };

    const cleanup = () => {
      if (pipedInputTimerId) {
        clearTimeout(pipedInputTimerId);
        pipedInputTimerId = null;
      }
      process.stdin.removeListener('readable', onReadable);
      process.stdin.removeListener('end', onEnd);
      process.stdin.removeListener('close', onEnd);
      process.stdin.removeListener('error', onError);
    };

    process.stdin.on('readable', onReadable);
    process.stdin.on('end', onEnd);
    process.stdin.on('close', onEnd);
    process.stdin.on('error', onError);
  });
}
