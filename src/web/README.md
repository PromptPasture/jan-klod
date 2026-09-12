# `src/web` — the browser client

A dependency-light TypeScript SPA, served by the core over its existing
REST + SSE surface. Built with `npm run build`; the output is `dist/app.js`.

## Dependencies, and why each one is here

The roadmap's ground rule is that the build pipeline sits **outside** the
runtime sandbox: nothing about the WebAssembly boundary protects it, so a
compromised build-time package runs with the privileges of whoever builds.
That makes the dependency count a design constraint rather than a preference,
and it is why each one is named here rather than left to `package.json`.

| Package | Why |
|---|---|
| `typescript` | `## Scope` asks for TypeScript. Used for type-checking only (`tsc --noEmit`); it never emits the shipped file. |
| `esbuild` | Bundles and minifies to one ES module. Chosen over Vite because Vite brings Rollup and a dev-server tree for features this does not need — the core serves the page, so there is no dev server to justify. |

**Two packages, and both are `devDependencies`.** There are **no runtime
dependencies at all**: `dist/app.js` contains this repository's code and
nothing else. That is the property worth not losing — a third-party runtime
dependency would be code shipped to a browser that the sandbox story says
nothing about.

### On the lockfile's 28 entries

`package-lock.json` lists 28 packages, which looks at odds with "two". Twenty-six
of them are `@esbuild/<platform>` — esbuild ships its compiler as a per-platform
binary and declares them all as optional so npm installs the one that matches.
Only one lands on disk. The count is worth stating plainly because a
supply-chain scan (#120) will report all 26, and somebody reading that report
should know what they are rather than concluding the tree grew.

## Layout

- `src/main.ts` — entry point.
- `dist/` — build output, git-ignored. The core embedding it is #119.

## Type checking

`strict`, plus `noUncheckedIndexedAccess` and `exactOptionalPropertyTypes`.
The client parses JSON off a wire and indexes into arrays of SSE frames, which
is exactly where a lax `tsconfig` stops being a preference: a missing frame or
an absent field should be a compile error here rather than an `undefined` in a
browser.
