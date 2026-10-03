# `interceptor-guardrails`

Content policy as rules an operator writes. The rest of the interceptor set
decides routing, context, tool choice and permission; this one looks at what a
turn *says* — the arguments of a tool call, the model's output, the answer, and
the messages heading for the provider.

Rules are data. Nothing here executes what you write in `config.yaml`; the only
thing read from it is text, and the only thing done with that text is matching.

## The four phases

| Phase | What it sees | What a match can do |
|---|---|---|
| `tool-call` | the call's arguments | refuse it, or ask you |
| `after-response` | the model's raw output | redact it, or refuse the turn |
| `finalize` | the assembled answer | redact it, or refuse the turn |
| `select-context` | messages on their way to the provider | redact them, or refuse to send |

`finalize` runs the same rules as `after-response` on purpose: text assembled
after the response was inspected has not been through them, and `finalize` is
the last point before the transcript. `select-context` is the only phase that
can keep matching text from leaving the machine at all, so a rule that blocks
there refuses the request rather than sending a redacted version of it.

## The rules

Both lists are optional, and both live under this extension's own key in
`config.yaml`. With neither configured the extension changes nothing: a content
filter nobody asked for is a surprise.

```yaml
deny-tool-arguments:
  - pattern: "rm +-rf"          # required
    tool: shell                 # optional; every tool when absent
    reason: "recursive delete"  # optional; shown to whoever is refused
    decision: block             # optional; `block` or `ask`, default `block`

redact:
  - pattern: "sk-[A-Za-z0-9]{16,}"  # required
    with: "[redacted]"              # optional; this is the default
    reason: "an API key"            # optional; shown when the rule refuses
    decision: replace               # optional; `replace` or `block`, default `replace`
```

- **A rule with no `reason` still explains itself** — the default names the
  pattern that fired.
- **`deny-tool-arguments` takes the first rule that matches**, in the order you
  wrote them. An order you chose is more predictable than a precedence invented
  here.
- **`redact` composes**: every `replace` rule runs over the text, and every
  occurrence is replaced, not just the first.
- **A blocking rule wins over redaction** wherever both match. Once you have
  said this text may not pass, a partially rewritten version of it is not what
  you asked for.
- **`ask` is not available on a `redact` rule.** There is nobody to ask
  mid-stream, so it is rejected as a configuration error rather than accepted
  as a rule that never fires.

## What the patterns can and cannot say

The engine is the [`regex`](https://docs.rs/regex) crate, which matches in
linear time and does not backtrack. That is the point: matching sits on the
path of every tool call and every response, so a pattern that could be made to
hang on crafted input would be a denial-of-service surface inside the thing
meant to prevent them.

The cost of that guarantee is that **lookaround (`(?=…)`, `(?!…)`) and
backreferences (`\1`) are not supported**. A pattern using them is refused when
the rules load — it does not silently become a rule that never matches.

Everything else in `regex`'s syntax works, including character classes,
repetition, alternation, anchors, and inline flags such as `(?i)` for
case-insensitive matching.

## When a rule is wrong

A malformed rule — an unparseable pattern, a missing `pattern`, an
unrecognised `decision` — makes the **whole rule set invalid**, and every
dispatch then reports an internal error. Host policy decides what that means
per phase (`wit/interceptor.wit`): **fail closed at `tool-call`**, log and
proceed elsewhere. So a typo stops tool calls until it is fixed, loudly, rather
than leaving a guardrail that silently is not there.

The error is logged at `init` with the offending value named.
