import {readFile} from 'node:fs/promises';

// One static artifact is immutable for the duration of a validation run.
export function createHtmlStore(read = readFile) {
  const documents = new Map();
  return async (file) => {
    if (!documents.has(file)) {
      const html = await read(file, 'utf8');
      const ids = [...html.matchAll(/\sid="([^"]+)"/g)].map((match) => match[1]);
      documents.set(file, {html, ids});
    }
    return documents.get(file);
  };
}
