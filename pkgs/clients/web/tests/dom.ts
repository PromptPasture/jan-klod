/**
 * The smallest DOM the client actually uses.
 *
 * `app.ts` touches six APIs — `createElement`, `textContent`, `dataset`,
 * `append`, `replaceChildren`, `addEventListener` — and nothing else. That is
 * what makes a stub defensible here: it is not standing in for a browser, it is
 * standing in for six methods, and a reader can check the list against the file.
 *
 * **What this deliberately does not test**: rendering, layout, event
 * propagation, or anything a real browser does differently. If those become the
 * risk, the answer is a real browser (Playwright), not a bigger stub — a stub
 * that grows to imitate a DOM is the thing that passes while the client is
 * broken. See `README.md`.
 */

export class StubElement {
  textContent = "";
  readonly dataset: Record<string, string> = {};
  readonly children: StubElement[] = [];
  readonly listeners: Record<string, (() => void)[]> = {};
  type = "";
  value = "";

  append(...nodes: StubElement[]): void {
    this.children.push(...nodes);
  }

  replaceChildren(...nodes: StubElement[]): void {
    this.children.length = 0;
    this.children.push(...nodes);
  }

  addEventListener(event: string, handler: (e?: unknown) => void): void {
    (this.listeners[event] ??= []).push(handler as () => void);
  }

  /** Fire a listener the way a user would. */
  click(): void {
    for (const handler of this.listeners["click"] ?? []) handler();
  }

  /** Every descendant's text, for asserting on what the user would see. */
  text(): string {
    return [this.textContent, ...this.children.map((c) => c.text())]
      .filter(Boolean)
      .join("\n");
  }

  find(predicate: (el: StubElement) => boolean): StubElement | undefined {
    if (predicate(this)) return this;
    for (const child of this.children) {
      const found = child.find(predicate);
      if (found) return found;
    }
    return undefined;
  }
}

export function installDom(): void {
  const g = globalThis as Record<string, unknown>;
  g["document"] = { createElement: () => new StubElement() };
  // `api.ts` reads the token on every request, so this is not optional
  // furniture — without it the first call throws before reaching the wire.
  const store = new Map<string, string>();
  g["sessionStorage"] = {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
  };
}
