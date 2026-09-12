/**
 * The browser client's entry point: wire the DOM to [`App`].
 *
 * No framework — see `README.md` for why the runtime dependency count is zero
 * and what that buys.
 */

import { App } from "./app.js";
import * as api from "./api.js";

/** Ask for the token once, if the gateway wants one. */
async function ensureToken(): Promise<void> {
  if (api.getToken()) return;
  const probe = await fetch("/sessions");
  // 401 is the gateway saying `JAN_KLOD_TOKEN` is set. Anything else means it
  // is not, and asking would be a box the user cannot fill in usefully.
  if (probe.status !== 401) return;
  const token = window.prompt("This gateway needs its token (JAN_KLOD_TOKEN):");
  if (token) api.setToken(token);
}

function element(id: string): HTMLElement {
  const found = document.getElementById(id);
  if (!found) throw new Error(`the page is missing #${id}`);
  return found;
}

export async function start(): Promise<void> {
  await ensureToken();
  const app = new App({
    sessions: element("sessions"),
    transcript: element("transcript"),
    status: element("status"),
    prompt: element("prompt"),
  });

  element("new-session").addEventListener("click", () => void app.create());
  element("cancel").addEventListener("click", () => app.cancel());

  const form = element("composer") as HTMLFormElement;
  const input = element("message") as HTMLInputElement;
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    void app.send(text);
  });

  await app.refreshSessions();
}

if (typeof document !== "undefined" && document.getElementById("app")) {
  void start();
}
