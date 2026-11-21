// The parts' styles reach only the parts: every rule is inside the engine's
// root or names one of the parts' classes; the page's own rules are page.css.
import { describe, expect, it } from "vitest";

const sheets = import.meta.glob<string>("./styles/*.css", { eager: true, query: "?raw", import: "default" });

/** Each rule's selectors, split at top-level commas, outside at-rules' own headers. */
function selectors(css: string): string[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const found: string[] = [];
  for (const [, head] of text.matchAll(/([^{}]+)\{/g)) {
    const h = head.trim();
    if (h.startsWith("@") || /^((from|to|\d+%)\s*,?\s*)+$/.test(h)) continue;
    let depth = 0;
    let part = "";
    for (const c of h) {
      if (c === "(") depth++;
      if (c === ")") depth--;
      if (c === "," && depth === 0) {
        found.push(part.trim());
        part = "";
      } else part += c;
    }
    found.push(part.trim());
  }
  return found;
}

const scoped = (selector: string) => /^(\.|:root\b|:where\(\.chirality\)|[a-z]+\.[a-z])/.test(selector);

describe("the parts' styles", () => {
  it("stay inside the engine's parts", () => {
    const loose = Object.entries(sheets)
      .filter(([path]) => !path.endsWith("/page.css"))
      .flatMap(([path, css]) => selectors(css).filter((s) => !scoped(s)).map((s) => `${path}: ${s}`));
    expect(loose).toEqual([]);
    expect(Object.keys(sheets)).toContain("./styles/page.css");
  });
});
