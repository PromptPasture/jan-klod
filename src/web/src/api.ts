/**
 * The core's REST + SSE surface, as this client uses it.
 *
 * Seven routes, and **cancel is not one of them**: a turn is cancelled by
 * dropping the SSE connection, which the conductor turns into `Flow::Stop`.
 * That is why `send` takes an `AbortSignal` rather than offering a `cancel()`
 * — there is nothing to call, and pretending otherwise would invent a route
 * the core does not serve.
 */

import { FrameReader, parseFrame, type Frame } from "./frames.ts";

/** Where the token lives, when there is one. */
const TOKEN_KEY = "jan-klod-token";

/**
 * `sessionStorage`, not `localStorage`.
 *
 * The token is the whole of the gateway's auth, so it should not outlive the
 * tab that was given it. `localStorage` would leave it on the machine until
 * something cleared it.
 */
export function setToken(token: string): void {
  sessionStorage.setItem(TOKEN_KEY, token);
}

export function getToken(): string | null {
  return sessionStorage.getItem(TOKEN_KEY);
}

function headers(extra: Record<string, string> = {}): Record<string, string> {
  const token = getToken();
  return token ? { ...extra, Authorization: `Bearer ${token}` } : extra;
}

export interface SessionSummary {
  id: string;
  preview?: string;
}

export interface TranscriptMessage {
  seq: number;
  role: string;
  content: string;
  "tool-call-id"?: string;
}

export async function listSessions(): Promise<SessionSummary[]> {
  const res = await fetch("/sessions", { headers: headers() });
  if (!res.ok) throw new Error(`listing sessions: ${res.status}`);
  const body: unknown = await res.json();
  const sessions = (body as { sessions?: unknown }).sessions;
  return Array.isArray(sessions) ? (sessions as SessionSummary[]) : [];
}

export async function createSession(): Promise<string> {
  const res = await fetch("/sessions", { method: "POST", headers: headers() });
  if (!res.ok) throw new Error(`creating a session: ${res.status}`);
  const body = (await res.json()) as { id?: string };
  if (!body.id) throw new Error("the core returned a session with no id");
  return body.id;
}

/** Resume: the transcript, oldest first. `seq` is what `session/fork` takes. */
export async function getSession(id: string): Promise<TranscriptMessage[]> {
  const res = await fetch(`/session/${encodeURIComponent(id)}`, {
    headers: headers(),
  });
  if (!res.ok) throw new Error(`reading a session: ${res.status}`);
  const body = (await res.json()) as { messages?: TranscriptMessage[] };
  return body.messages ?? [];
}

/** Answer a pending confirmation. The turn is parked until this lands. */
export async function answer(session: string, reply: string): Promise<void> {
  const res = await fetch(`/session/${encodeURIComponent(session)}/answer`, {
    method: "POST",
    headers: headers({ "Content-Type": "application/json" }),
    body: JSON.stringify({ answer: reply }),
  });
  if (!res.ok) throw new Error(`answering: ${res.status}`);
}

/**
 * Run a turn, calling `onFrame` for each SSE frame as it arrives.
 *
 * Aborting `signal` drops the connection, which is how a turn is cancelled.
 * An unknown frame kind is reported through `onUnknown` rather than ignored:
 * it means the core is ahead of this client, and a user should see that once
 * rather than never.
 */
export async function send(
  session: string,
  message: string,
  onFrame: (frame: Frame) => void,
  signal: AbortSignal,
  onUnknown: (kind: string) => void = () => {},
): Promise<void> {
  const res = await fetch(`/session/${encodeURIComponent(session)}/message`, {
    method: "POST",
    headers: headers({
      "Content-Type": "application/json",
      Accept: "text/event-stream",
    }),
    body: JSON.stringify({ message }),
    signal,
  });
  if (!res.ok || !res.body) throw new Error(`starting a turn: ${res.status}`);

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  const frames = new FrameReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    for (const raw of frames.push(decoder.decode(value, { stream: true }))) {
      const frame = parseFrame(raw.kind, raw.data);
      if (frame) onFrame(frame);
      else onUnknown(raw.kind);
    }
  }
}
