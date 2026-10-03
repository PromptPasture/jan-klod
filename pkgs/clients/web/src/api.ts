/**
 * The core's REST + SSE surface, as this client uses it.
 *
 * Nine routes, and **cancel is not one of them**: a turn is cancelled by
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

/** One argument a contributed command declares. */
export interface ContributedArgument {
  name: string;
  description: string;
  required: boolean;
}

/** A command an extension contributed. */
export interface ContributedCommand {
  name: string;
  title: string;
  description: string;
  arguments: ContributedArgument[];
}

/** A short piece of state an extension wants visible. */
export interface ContributedStatus {
  name: string;
  text: string;
  detail: string;
}

/** Everything one extension contributes. */
export interface Contributions {
  extension: string;
  commands: ContributedCommand[];
  "status-items": ContributedStatus[];
}

/** What invoking one answered. */
export interface InvokeOutcome {
  text: string;
  /** Whether the set moved, so this client should read it again. */
  "contributions-changed": boolean;
}

/**
 * What the extensions contribute to this client's interface.
 *
 * **Read, not pushed.** Over stdio the core announces this at the handshake
 * and again when it changes; there is no handshake here and no stream outside
 * a turn, so a browser asks — at start-up, and again whenever an invocation
 * reports that the set moved.
 *
 * Every string in the answer is written by a sandboxed extension. Nothing
 * here escapes it, because escaping belongs where the text meets the DOM;
 * what this owes the caller is not pretending it is safe.
 */
export async function contributions(): Promise<Contributions[]> {
  const res = await fetch("/contributions", { headers: headers() });
  if (!res.ok) throw new Error(`reading contributions: ${res.status}`);
  const body = (await res.json()) as { extensions?: Contributions[] };
  return body.extensions ?? [];
}

/**
 * Run a contributed command.
 *
 * `404` is a name nobody contributes — the caller's mistake, and worth
 * distinguishing from an extension that ran and failed (`500`), because one
 * means "this client is out of date" and the other "that extension is unwell".
 */
export async function invokeContribution(
  extension: string,
  name: string,
  args: { name: string; value: string }[] = [],
): Promise<InvokeOutcome> {
  const res = await fetch("/contributions/invoke", {
    method: "POST",
    headers: headers({ "Content-Type": "application/json" }),
    body: JSON.stringify({ extension, name, arguments: args }),
  });
  if (res.status === 404) throw new Error(`no extension contributes ${name}`);
  if (!res.ok) throw new Error(`invoking ${name}: ${res.status}`);
  return (await res.json()) as InvokeOutcome;
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
