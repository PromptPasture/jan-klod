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

import { readFileSync } from "node:fs";

import { installDom, StubElement } from "./dom.ts";
installDom();

const api = await import("../src/api.ts");
const { App } = await import("../src/app.ts");

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

function view() {
  return {
    sessions: new StubElement(),
    transcript: new StubElement(),
    status: new StubElement(),
    prompt: new StubElement(),
    contributions: new StubElement(),
  };
}

test("a contributed command is a button and a status item is a line", async () => {
  const { restore } = stubFetch(
    () => new Response(JSON.stringify(CONTRIBUTED), { status: 200 }),
  );
  try {
    const v = view();
    await new App(v as never).refreshContributions();
    const rendered = v.contributions.text();
    assert.match(rendered, /prompt — show the standing instructions/);
    assert.match(rendered, /interceptor\.system built-in/);
  } finally {
    restore();
  }
});

test("clicking a contributed command invokes it by extension and name", async () => {
  const calls: Call[] = [];
  const real = globalThis.fetch;
  globalThis.fetch = (async (input: string, init?: RequestInit) => {
    calls.push({
      url: input,
      method: init?.method ?? "GET",
      body: init?.body as string | undefined,
    });
    if (input === "/contributions/invoke") {
      return new Response(
        JSON.stringify({ text: "the prompt", "contributions-changed": false }),
        { status: 200 },
      );
    }
    return new Response(JSON.stringify(CONTRIBUTED), { status: 200 });
  }) as typeof fetch;
  try {
    const v = view();
    await new App(v as never).refreshContributions();
    const button = v.contributions.children[0];
    assert.ok(button, "the command rendered as something clickable");
    button.click();
    // The click handler is async; let its promise settle.
    await new Promise((resolve) => setTimeout(resolve, 0));
    const invoke = calls.find((c) => c.url === "/contributions/invoke");
    assert.ok(invoke, "clicking it invoked something");
    const sent = JSON.parse(invoke.body ?? "{}") as Record<string, unknown>;
    assert.equal(sent["extension"], "interceptor.system");
    assert.equal(sent["name"], "prompt");
    assert.equal(v.status.textContent, "the prompt");
  } finally {
    globalThis.fetch = real;
  }
});

/**
 * Markup in a contributed label is text, and the label is shown whole.
 *
 * The stub DOM cannot parse HTML, so this alone would not catch an
 * `innerHTML`; the test below is what does. This one checks the other half —
 * that nothing strips or mangles a label on the way, so an honest extension
 * sees what it wrote and a hostile one gains nothing by hiding in markup.
 */
test("markup in a label is carried as text, not interpreted or dropped", async () => {
  const hostile = {
    extensions: [
      {
        extension: "interceptor.system",
        commands: [
          {
            name: "<script>alert(1)</script>",
            title: "t",
            description: "<img src=x onerror=alert(1)>",
            arguments: [],
          },
        ],
        "status-items": [],
      },
    ],
  };
  const { restore } = stubFetch(
    () => new Response(JSON.stringify(hostile), { status: 200 }),
  );
  try {
    const v = view();
    await new App(v as never).refreshContributions();
    const rendered = v.contributions.text();
    assert.match(rendered, /<script>alert\(1\)<\/script>/);
    assert.match(rendered, /<img src=x onerror=alert\(1\)>/);
  } finally {
    restore();
  }
});

/**
 * The escaping rule, checked where a stub DOM cannot check it: the client's
 * own source. Every string it draws goes through `textContent`, so a browser
 * renders markup as characters. One `innerHTML` would undo that silently and
 * no assertion over a stub would notice.
 *
 * Grep-style, like `src/tui/tests/sidebar_projection.rs`. Comments are
 * stripped first, for the reason that file gives about living outside the
 * code it checks: `app.ts` explains in prose *why* it never assigns HTML, and
 * a scan that read its own explanation would fail on a correct file. It did,
 * the first time this ran.
 */
test("the client never assigns HTML", async () => {
  const source = readFileSync(new URL("../src/app.ts", import.meta.url), "utf8")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\/\/.*/g, "");
  for (const forbidden of [
    "innerHTML",
    "outerHTML",
    "insertAdjacentHTML",
    "document.write",
  ]) {
    assert.ok(
      !source.includes(forbidden),
      `app.ts uses ${forbidden}: contributed text would stop being text`,
    );
  }
});
