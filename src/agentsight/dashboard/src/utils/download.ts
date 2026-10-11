/** Download a Blob while owning the temporary link and object URL. */
export function downloadBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  let link: HTMLAnchorElement | undefined;
  try {
    link = document.createElement('a');
    link.href = url;
    link.download = filename;
    document.body.appendChild(link);
    link.click();
  } finally {
    try {
      link?.remove();
    } finally {
      // Give browsers time to begin consuming the download before releasing it.
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    }
  }
}
