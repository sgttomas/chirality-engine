/** Controls that take their own keys: a shortcut set on the whole page leaves a key pressed on one to it. */
const CONTROLS = [
  "button",
  "a[href]",
  "input",
  "textarea",
  "select",
  "summary",
  "[contenteditable]:not([contenteditable='false'])",
  ...["button", "link", "menuitem", "option", "tab", "checkbox", "radio", "switch", "textbox", "combobox", "listbox"].map((r) => `[role='${r}']`),
].join(", ");

/** Whether a page-wide shortcut is to act on this key: not when a control has it, or something took it first. */
export function forShortcut(e: KeyboardEvent): boolean {
  if (e.defaultPrevented || e.isComposing) return false;
  return !(e.target instanceof Element && e.target.closest(CONTROLS));
}
