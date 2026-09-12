/**
 * The browser client's entry point.
 *
 * Scaffold only — the client itself is the next slice of #118. What this file
 * establishes now is that the toolchain produces a bundle with **no
 * third-party runtime code in it**, which is the property `src/web/README.md`
 * explains and the one worth not losing later.
 */
export function mount(root: HTMLElement): void {
  root.textContent = "jan-klod";
}

const root = document.getElementById("app");
if (root) {
  mount(root);
}
