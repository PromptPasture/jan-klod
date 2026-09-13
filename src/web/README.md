# `src/web` — the browser client

Dependency-light TypeScript SPA served by core over REST + SSE. Built with `npm run build` → `dist/app.js`.

## Dependencies, and why each one is here

Build pipeline sits **outside** the sandbox (nothing protects it). Compromised build-time code runs with builder's privileges. Dependency count is a design constraint, not preference; named here (not just `package.json`).

| Package | Why |
|---|---|
| `typescript` | `## Scope` asks for TypeScript. Used for type-checking only (`tsc --noEmit`); it never emits the shipped file. |
| `esbuild` | Bundles and minifies to one ES module. Chosen over Vite because Vite brings Rollup and a dev-server tree for features this does not need — the core serves the page, so there is no dev server to justify. |

**Both `devDependencies`**—**no runtime deps**. `dist/app.js` is only repo code. Third-party runtime code shipped to browser breaks the sandbox story.

### On the lockfile's 28 entries

`package-lock.json` lists 28 (vs. "two"). 26 are `@esbuild/<platform>` — optional platform-specific binaries; npm installs one. Supply-chain scan (#120) reports all 26; know what they are (tree didn't grow).

## Layout

- `src/main.ts` — entry point.
- `dist/` — build output (git-ignored). Core embedding is #119.

## Type checking

`strict` + `noUncheckedIndexedAccess` + `exactOptionalPropertyTypes`. Client parses JSON wire and indexes SSE frame arrays—where lax tsconfig stops being preference. Missing frame/field = compile error (not runtime `undefined`).

## Tests

```sh
npm test
```

Node's built-in runner + `--experimental-strip-types` + ~40-line DOM stub. **No new deps**—the whole reason for this approach.

### Why not jsdom or Playwright

`## Scope` asked for cheapest-to-keep-green; both were costed:

- **jsdom**: 28→66 packages, 32 MB→58 MB `node_modules` (doubled for 5.4 kB client). Measured.
- **Playwright**: downloads browser (CI cache, version management). Buys real rendering (not where risk is now).

`app.ts` touches six DOM APIs: `createElement`, `textContent`, `dataset`, `append`, `replaceChildren`, `addEventListener`. Six-method stub is checkable; defensible, not merely cheap.

**Not tested**: rendering, layout, event propagation, CSS, browser-specific behavior. If risk shifts there, answer is Playwright (not bigger stub—growing stubs pass while client breaks).

### Two constraints this choice imposes on the source

Found by tests refusing to run; cheap to trip over again:

1. **No TypeScript parameter properties.** `--experimental-strip-types` removes
   annotations without running a compiler, and `constructor(private x: T)` is
   not an annotation — it emits an assignment. `App` uses a plain field for
   this reason.
2. **Imports end in `.ts`, not `.js`.** Node resolves the real file when it
   runs the sources directly; esbuild is happy either way. `tsconfig` sets
   `allowImportingTsExtensions` to match.

