/**
 * A full turn, offline: create a session, send a message, receive every frame
 * kind, answer an `ask`, and cancel.
 *
 * No browser and no network. `fetch` is a stub that plays the core's part —
 * the "canned provider" this slice's Acceptance line asks for — and the SSE
 * body is a real `ReadableStream`, so the client's chunk-splitting is exercised
 * rather than bypassed.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import { installDom, StubElement } from "./dom.ts";
installDom();

const { App } = await import("../src/app.ts");
const { FrameReader, parseFrame } = await import("../src/frames.ts");

/**
 * An SSE body, delivered in awkward chunks and **abortable**.
 *
 * Three bytes at a time, one chunk per `pull`: a frame — and a JSON payload —
 * is split across reads, so the client's buffering is exercised rather than
 * bypassed. A stream delivered whole in `start()` would never test it, and
 * would also never let a cancel land mid-turn, which is what the cancel test
 * needs. The first version of this stub did exactly that and the cancel test
 * failed for the right reason: the turn had already finished.
 */
function sseStream(
  frames: [string, unknown][],
  signal?: AbortSignal,
): ReadableStream<Uint8Array> {
  const text = frames
    .map(([kind, data]) => `event: ${kind}\ndata: ${JSON.stringify(data)}\n\n`)
    .join("");
  const bytes = new TextEncoder().encode(text);
  let at = 0;
  return new ReadableStream({
    async pull(controller) {
      // A real `fetch` body errors when its signal aborts; a stub that ignores
      // the signal would let a cancelled turn run to completion and report
      // `done`, which is not what a browser does.
      if (signal?.aborted) {
        controller.error(new DOMException("aborted", "AbortError"));
        return;
      }
      if (at >= bytes.length) {
        controller.close();
        return;
      }
      await Promise.resolve();
      controller.enqueue(bytes.slice(at, at + 3));
      at += 3;
    },
  });
}

interface Call {
  url: string;
  method: string;
  body?: string;
}

function stubFetch(frames: [string, unknown][]): {
  calls: Call[];
  restore: () => void;
} {
  const calls: Call[] = [];
  const real = globalThis.fetch;
  globalThis.fetch = (async (input: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    calls.push({ url: input, method, body: init?.body as string | undefined });
    if (input === "/sessions" && method === "POST") {
      return new Response(JSON.stringify({ id: "s1" }), { status: 200 });
    }
    if (input === "/sessions") {
      return new Response(JSON.stringify({ sessions: [] }), { status: 200 });
    }
    if (input.endsWith("/message")) {
      if (init?.signal?.aborted) throw new DOMException("aborted", "AbortError");
      return new Response(sseStream(frames, init?.signal ?? undefined), { status: 200 });
    }
    if (input.endsWith("/answer")) return new Response("{}", { status: 200 });
    return new Response(JSON.stringify({ messages: [] }), { status: 200 });
  }) as typeof fetch;
  return { calls, restore: () => (globalThis.fetch = real) };
}

function view() {
  return {
    sessions: new StubElement(),
    transcript: new StubElement(),
    status: new StubElement(),
    prompt: new StubElement(),
  };
}

test("a turn renders every frame kind the core can send", async () => {
  const frames: [string, unknown][] = [
    ["delta", { text: "hel" }],
    ["delta", { text: "lo" }],
    ["tool", { id: "c1", name: "fs" }],
    ["tool-result", { id: "c1", content: "file contents" }],
    ["warning", { message: "careful" }],
    ["done", { answer: "hello", agentic: true }],
  ];
  const { restore } = stubFetch(frames);
  const v = view();
  const app = new App(v);
  await app.create();
  await app.send("hi");
  restore();

  const shown = v.transcript.text();
  assert.match(shown, /hello/, "streamed deltas are joined, not overwritten");
  assert.match(shown, /→ fs/, "a tool invocation is visible");
  assert.match(shown, /file contents/, "a tool result is visible");
  assert.equal(v.status.textContent, "done");
});

test("an ask is answered on a second request while the turn is parked", async () => {
  const { calls, restore } = stubFetch([
    ["prompt", { question: "Write the file?", options: ["yes", "no", "always"], default: "no" }],
    ["done", { answer: "", agentic: true }],
  ]);
  const v = view();
  const app = new App(v);
  await app.create();
  await app.send("edit it");

  const button = v.prompt.find((el) => el.dataset["answer"] === "always");
  assert.ok(button, "the interceptor's own options are offered, including `always`");
  button.click();
  await new Promise((r) => setTimeout(r, 0));
  restore();

  const answered = calls.find((c) => c.url.endsWith("/answer"));
  assert.ok(answered, "answering goes to POST /session/:id/answer");
  assert.match(answered.body ?? "", /"always"/);
});

test("cancel drops the connection rather than calling a cancel route", async () => {
  const { calls, restore } = stubFetch([["done", { answer: "x", agentic: false }]]);
  const v = view();
  const app = new App(v);
  await app.create();
  app.cancel(); // no turn running: must not throw
  const running = app.send("hi");
  app.cancel();
  await running;
  restore();

  assert.equal(v.status.textContent, "cancelled");
  assert.ok(
    !calls.some((c) => c.url.includes("cancel")),
    "there is no cancel route — the conductor reads the disconnect as Flow::Stop",
  );
});

test("a chunk boundary inside a frame does not lose it", () => {
  const reader = new FrameReader();
  assert.deepEqual(reader.push("event: del"), []);
  assert.deepEqual(reader.push('ta\ndata: {"te'), []);
  const out = reader.push('xt":"hi"}\n\n');
  assert.equal(out.length, 1);
  assert.deepEqual(parseFrame(out[0]!.kind, out[0]!.data), { kind: "delta", text: "hi" });
});

test("an unknown frame is reported, not silently dropped", () => {
  assert.equal(parseFrame("thinking", "{}"), null);
});
