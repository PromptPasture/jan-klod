/**
 * The client: a session list, a transcript, an input, and a prompt dialog.
 *
 * Plain DOM. `src/tui/src/app.rs` is the reference for *what* a client must
 * handle — this is the same set of responsibilities with a different surface.
 */

import * as api from "./api.ts";
import type { Frame } from "./frames.ts";

export interface View {
  sessions: HTMLElement;
  transcript: HTMLElement;
  status: HTMLElement;
  prompt: HTMLElement;
  /** Where extensions' contributions are drawn (`wit/client-surface.wit`). */
  contributions: HTMLElement;
}

export class App {
  private session: string | null = null;
  private turn: AbortController | null = null;
  /** The assistant bubble being streamed into, if a turn is running. */
  private streaming: HTMLElement | null = null;

  private readonly view: View;

  // A plain field, not a `private readonly view: View` parameter property.
  // Node's `--experimental-strip-types` removes type annotations without
  // running a compiler, and a parameter property is not an annotation — it
  // emits an assignment. Using one makes this file unloadable by the test
  // runner, which is the price of testing with zero dependencies (README).
  constructor(view: View) {
    this.view = view;
  }

  async refreshSessions(): Promise<void> {
    const sessions = await api.listSessions();
    this.view.sessions.replaceChildren(
      ...sessions.map((s) => {
        const item = document.createElement("button");
        item.type = "button";
        item.textContent = s.preview ? `${s.id} — ${s.preview}` : s.id;
        item.dataset["session"] = s.id;
        item.addEventListener("click", () => void this.open(s.id));
        return item;
      }),
    );
  }

  async create(): Promise<void> {
    await this.open(await api.createSession());
    await this.refreshSessions();
  }

  /** Resume: replay what the session already holds before anything new. */
  async open(id: string): Promise<void> {
    this.session = id;
    this.view.transcript.replaceChildren();
    for (const message of await api.getSession(id)) {
      this.append(message.role, message.content);
    }
    this.status(`session ${id}`);
  }

  async send(message: string): Promise<void> {
    if (!this.session) throw new Error("no session is open");
    this.append("user", message);
    this.streaming = this.append("assistant", "");
    this.turn = new AbortController();
    this.status("…");
    try {
      await api.send(
        this.session,
        message,
        (frame) => this.onFrame(frame),
        this.turn.signal,
        (kind) => this.status(`unknown frame from the core: ${kind}`),
      );
    } catch (err) {
      // An abort is this client cancelling, not a failure — the conductor is
      // told by the disconnect itself, so there is nothing else to report.
      if (!(err instanceof DOMException && err.name === "AbortError")) {
        this.status(`turn failed: ${String(err)}`);
      }
    } finally {
      this.turn = null;
      this.streaming = null;
    }
  }

  /**
   * Cancel by dropping the connection. There is no cancel route: the conductor
   * reads the disconnect as `Flow::Stop`.
   */
  cancel(): void {
    this.turn?.abort();
    this.status("cancelled");
  }

  /**
   * One arm per frame kind, with an exhaustiveness assertion after the switch
   * so a frame added to the core and not handled here is a **compile error**
   * rather than a silent drop. See the note at the bottom: the `switch` on its
   * own does not give that, which is easy to believe and wrong.
   */
  private onFrame(frame: Frame): void {
    switch (frame.kind) {
      case "delta":
        if (this.streaming) this.streaming.textContent += frame.text;
        return;
      case "tool":
        this.append("tool", `→ ${frame.name}`);
        return;
      case "tool-result":
        this.append("tool", frame.content);
        return;
      case "warning":
        this.status(`warning: ${frame.message}`);
        return;
      case "prompt":
        this.ask(frame.question, frame.options, frame.default);
        return;
      case "done":
        if (this.streaming && !this.streaming.textContent) {
          this.streaming.textContent = frame.answer;
        }
        this.status(frame.agentic ? "done" : "done (answered inline)");
        return;
      case "error":
        this.status(`error: ${frame.error}`);
        return;
    }
    // Reached only if `Frame` grows a kind with no arm above — and then
    // `frame` is that kind rather than `never`, so this line does not compile.
    //
    // The `switch` alone does **not** give this: every arm returns, so a
    // missing case is simply a function that falls through, which TypeScript
    // accepts. Asserting it here is what turns "we handle them all" from a
    // comment into something the build checks — and the comment was wrong
    // until this line existed.
    const unhandled: never = frame;
    throw new Error(`unhandled frame: ${JSON.stringify(unhandled)}`);
  }

  /**
   * A confirmation, answered on a second request while the turn is parked.
   *
   * The buttons are the options the interceptor sent — not a fixed
   * yes/no — because the option set is the gate's to choose, and an "always"
   * that this client did not offer would be a grant the user never saw.
   */
  private ask(question: string, options: string[], fallback: string): void {
    const session = this.session;
    if (!session) return;
    const box = document.createElement("div");
    box.dataset["prompt"] = "1";
    const text = document.createElement("p");
    text.textContent = question;
    box.append(text);
    for (const option of options.length ? options : [fallback]) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = option;
      button.dataset["answer"] = option;
      button.addEventListener("click", () => {
        void api.answer(session, option);
        this.view.prompt.replaceChildren();
      });
      box.append(button);
    }
    this.view.prompt.replaceChildren(box);
  }

  /**
   * Draw what the extensions contribute: a button per command, a line per
   * status item.
   *
   * **Every string here was written by a sandboxed extension.** Each one is
   * assigned with `textContent`, so markup is text and a `<script>` in a
   * label is a label that reads `<script>`. Nothing is built with
   * `innerHTML`, and no contribution chooses a URL — there is no `href` here
   * to give one, and if a contribution ever needs a link that is a change to
   * `wit/client-surface.wit` rather than a decision this client makes.
   *
   * The extension's own id leads each row for the same reason the terminal
   * client leads with it: a claim rendered bare reads as this client's word
   * for something.
   */
  async refreshContributions(): Promise<void> {
    const sets = await api.contributions();
    const rows: HTMLElement[] = [];
    for (const set of sets) {
      for (const command of set.commands) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = `${command.name} — ${command.description}`;
        button.title = `${set.extension}: ${command.title}`;
        button.dataset["extension"] = set.extension;
        button.dataset["contribution"] = command.name;
        button.addEventListener("click", () => {
          void this.invoke(set.extension, command.name);
        });
        rows.push(button);
      }
      for (const item of set["status-items"]) {
        const line = document.createElement("div");
        line.dataset["status-item"] = item.name;
        line.textContent = `${set.extension} ${item.text}`;
        line.title = item.detail;
        rows.push(line);
      }
    }
    this.view.contributions.replaceChildren(...rows);
  }

  /**
   * Run a contribution and show what it answered.
   *
   * A set that moved is read again before the answer is shown, so a user is
   * never looking at an outdated menu while reading the result of the thing
   * that changed it.
   */
  private async invoke(extension: string, name: string): Promise<void> {
    try {
      const outcome = await api.invokeContribution(extension, name);
      if (outcome["contributions-changed"]) await this.refreshContributions();
      this.status(outcome.text || `${name} ran`);
    } catch (err) {
      this.status(err instanceof Error ? err.message : String(err));
    }
  }

  private append(role: string, content: string): HTMLElement {
    const line = document.createElement("div");
    line.dataset["role"] = role;
    line.textContent = content;
    this.view.transcript.append(line);
    return line;
  }

  private status(text: string): void {
    this.view.status.textContent = text;
  }
}
