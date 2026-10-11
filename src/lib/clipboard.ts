/** WebKitGTK may refuse the asynchronous clipboard; a selected text area always copies. `unavailable` says what to do instead. */
export async function copyText(text: string, unavailable: string) {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    const focused = document.activeElement as HTMLElement | null;
    const area = document.createElement("textarea");
    area.value = text;
    area.style.cssText = "position:fixed;opacity:0";
    document.body.append(area);
    area.select();
    const copied = document.execCommand("copy");
    area.remove();
    focused?.focus();
    if (!copied) throw new Error(unavailable);
  }
}
