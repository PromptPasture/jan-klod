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

## Tests

```sh
npm test
```

Node's built-in runner over `--experimental-strip-types`, with a ~40-line DOM
stub in `tests/dom.ts`. **No new dependencies**, which is the whole reason for
the shape.

### Why not jsdom or Playwright

`## Scope` asked for "whichever is cheaper to keep green", so both were costed
rather than guessed:

- **jsdom** takes the dependency tree from **28 packages to 66** and
  `node_modules` from 32 MB to 58 MB — more than doubling it, to test 5.4 kB of
  client. Measured, not estimated.
- **Playwright** downloads a browser, which is a CI cache and a version to keep
  in step. It buys real rendering, which is not currently where the risk is.

`app.ts` touches exactly six DOM APIs — `createElement`, `textContent`,
`dataset`, `append`, `replaceChildren`, `addEventListener`. A stub for six
methods is checkable by reading it against the file; that is what makes this
defensible rather than merely cheap.

**What it does not test**: rendering, layout, event propagation, CSS, or
anything a browser does differently. If those become the risk the answer is
Playwright, **not a bigger stub** — a stub that grows to imitate a DOM is the
thing that passes while the client is broken.

### Two constraints this choice imposes on the source

Both were found by the tests refusing to run, and both are cheap to trip over
again:

1. **No TypeScript parameter properties.** `--experimental-strip-types` removes
   annotations without running a compiler, and `constructor(private x: T)` is
   not an annotation — it emits an assignment. `App` uses a plain field for
   this reason.
2. **Imports end in `.ts`, not `.js`.** Node resolves the real file when it
   runs the sources directly; esbuild is happy either way. `tsconfig` sets
   `allowImportingTsExtensions` to match.

