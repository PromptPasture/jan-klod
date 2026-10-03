/**
 * `tool-hello-ts` — the TypeScript twin of `tool-hello`.
 *
 * The same tool, in a second language, against the same `wit/`. It exists to
 * keep the polyglot claim honest: `docs/concepts/architecture.md` says an
 * extension can be written in any `wit-bindgen` language, and until this guest
 * that rested on one TinyGo spike built for the Slice 1a gate.
 *
 * ## What is different from the Rust twin, and what is not
 *
 * Not different: the world, the interfaces, the host's treatment of it. The
 * host loads this the way it loads any component — no special case.
 *
 * Different: **size**. ComponentizeJS embeds a JavaScript engine, so this
 * component is ~12.7 MB against `tool-hello`'s 55 KB. That is the price of the
 * language, not of this guest, and it is why `make extensions` builds this one
 * only when the toolchain is present.
 *
 * ## Exports, and why they are spelled this way
 *
 * `jco` lowers WIT's kebab-case to lowerCamelCase: interface `tool-callable`
 * becomes the export `toolCallable`, record field `arguments-schema` becomes
 * `argumentsSchema`. There are no generated bindings to import — the component
 * is built *from* this module against the WIT, so the shapes below are
 * hand-written to match and the componentize step is what checks them.
 */

/** The world's `extension-lifecycle` export. */
export const extensionLifecycle = {
  init(_context: { id: string; version: string }): void {},
  start(): void {},
  stop(): void {},
  /** `health-status` is an enum; jco lowers its cases to strings. */
  health(): string {
    return "up";
  },
};

/** The world's `tool-callable` export. */
export const toolCallable = {
  meta() {
    return {
      name: "hello-ts",
      description: "Returns a greeting. The TypeScript twin of tool-hello.",
      argumentsSchema: JSON.stringify({
        type: "object",
        properties: { name: { type: "string" } },
      }),
    };
  },

  /**
   * `arguments` is a JSON string and the result is a JSON string — the
   * interface says so, and a tool that returned bare prose would be readable
   * to a person and not to the loop.
   */
  invoke(args: string): string {
    let who = "world";
    try {
      const parsed: unknown = JSON.parse(args || "{}");
      if (parsed && typeof parsed === "object" && "name" in parsed) {
        const named = (parsed as { name?: unknown }).name;
        if (typeof named === "string" && named.length > 0) who = named;
      }
    } catch {
      // A malformed argument object is the caller's, and greeting the world
      // is a better answer than a trap that the loop reports as a tool crash.
    }
    return JSON.stringify({ greeting: `hello, ${who}, from TypeScript` });
  },
};
