/** Clipboard copying shared by dashboard buttons and exported text. */

function fallbackCopy(text: string, done: () => void) {
  const el = document.createElement('textarea');
  el.value = text;
  el.style.position = 'fixed';
  el.style.opacity = '0';
  document.body.appendChild(el);
  let copied = false;
  try {
    el.focus();
    el.select();
    copied = document.execCommand('copy');
  } catch {
    copied = false;
  } finally {
    document.body.removeChild(el);
  }
  if (copied) done();
}

/** Copy text, falling back to execCommand in insecure HTTP contexts. */
export function copyText(text: string, done: () => void) {
  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).then(done).catch(() => fallbackCopy(text, done));
  } else {
    fallbackCopy(text, done);
  }
}
