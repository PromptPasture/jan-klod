/**
 * The SSE frames the core emits, and the guarantee that this client answers
 * every one of them.
 *
 * # Why this file is a discriminated union and not `any`
 *
 * `jan_klod_protocol::SSE_FRAME_KINDS` names seven frames, and its own docs
 * say what the two tests around it are for: `core/tests/protocol_events.rs`
 * proves the core produces exactly these, and a client test proves a client
 * "has an answer for each" — *"shipping a client that ignores it fails the
 * second"*. This repository has already shipped that defect: the TUI reported
 * every `tool-result` as an unknown frame, and the stdio transport dropped them
 * without a word.
 *
 * So the union below is exhaustive, and `handleFrame` switches on it without a
 * default arm. Adding a frame to the core and not here is a **compile error**,
 * which is the cheapest place for this particular mistake to be caught.
 *
 * # These are not the protocol's notification names
 *
 * The SSE projection keeps its own spellings — `delta` for `text-delta`, `tool`
 * for `tool-invoked`, `prompt` for `ask`. The contract's docs say plainly: *"Do
 * not 'fix' either side to agree with the other."* They are held together by a
 * compatibility test, not by matching strings.
 */

/** Every SSE event name the core emits. Mirrors `SSE_FRAME_KINDS`. */
export const FRAME_KINDS = [
  "delta",
  "done",
  "error",
  "prompt",
  "tool",
  "tool-result",
  "warning",
] as const;

export type FrameKind = (typeof FRAME_KINDS)[number];

/** One frame, with the payload shape `serve::sse_frame` actually writes. */
export type Frame =
  | { kind: "delta"; text: string }
  | { kind: "done"; answer: string; agentic: boolean }
  | { kind: "error"; error: string }
  | { kind: "prompt"; question: string; options: string[]; default: string }
  | { kind: "tool"; id: string; name: string }
  | { kind: "tool-result"; id: string; content: string }
  | { kind: "warning"; message: string };

function isFrameKind(value: string): value is FrameKind {
  return (FRAME_KINDS as readonly string[]).includes(value);
}

/**
 * Parse one `event:`/`data:` pair.
 *
 * Returns `null` for a kind this client does not know, and the caller reports
 * it rather than dropping it silently — an unknown frame means the core is
 * ahead of this client, which a user should see once rather than never.
 */
export function parseFrame(kind: string, data: string): Frame | null {
  if (!isFrameKind(kind)) return null;
  const payload: unknown = JSON.parse(data);
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as Record<string, unknown>;
  switch (kind) {
    case "delta":
      return { kind, text: String(p["text"] ?? "") };
    case "done":
      return {
        kind,
        answer: String(p["answer"] ?? ""),
        agentic: Boolean(p["agentic"]),
      };
    case "error":
      return { kind, error: String(p["error"] ?? "") };
    case "prompt":
      return {
        kind,
        question: String(p["question"] ?? ""),
        options: Array.isArray(p["options"]) ? p["options"].map(String) : [],
        default: String(p["default"] ?? ""),
      };
    case "tool":
      return { kind, id: String(p["id"] ?? ""), name: String(p["name"] ?? "") };
    case "tool-result":
      return {
        kind,
        id: String(p["id"] ?? ""),
        content: String(p["content"] ?? ""),
      };
    case "warning":
      return { kind, message: String(p["message"] ?? "") };
  }
}

/**
 * Split an SSE byte stream into frames.
 *
 * Frames are `event: <kind>\ndata: <json>\n\n`, and a chunk boundary can fall
 * anywhere — including mid-JSON — so this buffers until it has a blank line
 * rather than assuming a read is a frame.
 */
export class FrameReader {
  private buffer = "";

  push(chunk: string): { kind: string; data: string }[] {
    this.buffer += chunk;
    const out: { kind: string; data: string }[] = [];
    let split = this.buffer.indexOf("\n\n");
    while (split !== -1) {
      const block = this.buffer.slice(0, split);
      this.buffer = this.buffer.slice(split + 2);
      const kind = block.match(/^event: (.*)$/m)?.[1];
      const data = block.match(/^data: (.*)$/m)?.[1];
      // A comment-only block (`:` keep-alive) has neither, and is not a frame.
      if (kind !== undefined && data !== undefined) out.push({ kind, data });
      split = this.buffer.indexOf("\n\n");
    }
    return out;
  }
}
