/**
 * Reading and invoking what extensions contribute, over the two routes the
 * core added for a browser.
 *
 * No network: `fetch` is a stub playing the core's part, the same way
 * `turn.test.ts` does. What is under test is this client's half — the shapes
 * it sends, and what it makes of the answers.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import { installDom } from "./dom.ts";
installDom();

const api = await import("../src/api.ts");

/** One extension contributing a command and a status item. */
const CONTRIBUTED = {
  extensions: [
    {
      extension: "interceptor.system",
      commands: [
        {
          name: "prompt",
          title: "System prompt",
          description: "show the standing instructions",
          arguments: [
            { name: "verbose", description: "the whole text", required: false },
          ],
        },
      ],
      "status-items": [
        { name: "prompt-source", text: "built-in", detail: "no prompt is set" },
      ],
    },
  ],
};

interface Call {
  url: string;
  method: string;
  body?: string;
}

function stubFetch(answer: (url: string) => Response): {
  calls: Call[];
  restore: () => void;
} {
  const calls: Call[] = [];
  const real = globalThis.fetch;
  globalThis.fetch = (async (input: string, init?: RequestInit) => {
    calls.push({
      url: input,
      method: init?.method ?? "GET",
      body: init?.body as string | undefined,
    });
    return answer(input);
  }) as typeof fetch;
  return { calls, restore: () => (globalThis.fetch = real) };
}

test("the contributed set is read from the core, not assumed", async () => {
  const { calls, restore } = stubFetch(
    () => new Response(JSON.stringify(CONTRIBUTED), { status: 200 }),
  );
  try {
    const sets = await api.contributions();
    assert.equal(calls[0]?.url, "/contributions");
    assert.equal(sets.length, 1);
    assert.equal(sets[0]?.extension, "interceptor.system");
    assert.equal(sets[0]?.commands[0]?.name, "prompt");
    assert.equal(sets[0]?.["status-items"][0]?.text, "built-in");
  } finally {
    restore();
  }
});

/** A core with no contributing extensions answers with an empty list, and
 * that is a normal answer rather than a failure. */
test("a core that contributes nothing yields an empty set", async () => {
  const { restore } = stubFetch(
    () => new Response(JSON.stringify({ extensions: [] }), { status: 200 }),
  );
  try {
    assert.deepEqual(await api.contributions(), []);
  } finally {
    restore();
  }
});

test("invoking one sends the extension and the name together", async () => {
  const { calls, restore } = stubFetch(
    () =>
      new Response(
        JSON.stringify({ text: "the prompt", "contributions-changed": false }),
        { status: 200 },
      ),
  );
  try {
    const outcome = await api.invokeContribution("interceptor.system", "prompt", [
      { name: "verbose", value: "true" },
    ]);
    assert.equal(calls[0]?.url, "/contributions/invoke");
    assert.equal(calls[0]?.method, "POST");
    // Both halves, because two extensions may contribute the same name.
    const sent = JSON.parse(calls[0]?.body ?? "{}") as Record<string, unknown>;
    assert.equal(sent["extension"], "interceptor.system");
    assert.equal(sent["name"], "prompt");
    assert.deepEqual(sent["arguments"], [{ name: "verbose", value: "true" }]);
    assert.equal(outcome.text, "the prompt");
    assert.equal(outcome["contributions-changed"], false);
  } finally {
    restore();
  }
});

/**
 * A 404 means this client offered something the core does not have — it is
 * out of date, not broken — and the message says so rather than reporting a
 * bare status number.
 */
test("invoking something the core does not contribute says which name failed", async () => {
  const { restore } = stubFetch(
    () => new Response(JSON.stringify({ error: "no" }), { status: 404 }),
  );
  try {
    await assert.rejects(
      () => api.invokeContribution("interceptor.system", "gone"),
      /gone/,
    );
  } finally {
    restore();
  }
});
