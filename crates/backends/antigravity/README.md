# nanocode-backend-antigravity

A nanocode harness that reproduces the engine of Google Antigravity CLI (`agy` 1.5.13): its system prompt,
its engine tools, its per-turn reminders, its compaction and its Gemini request shape. Subagents, Code Mode and MCP
stay nanocode's own, as in the other harnesses.

## Selecting it

```
nanocode --harness antigravity                 # gemini-4-neon, thinking High
nanocode --harness antigravity -m gemini-3-pro
nanocode -m gemini-4-neon                      # a gemini-* model picks this harness
```

Credentials, read in this order:

| Variable | Use |
| --- | --- |
| `AGY_LLM_GATEWAY_URL`, `AGY_LLM_GATEWAY_API_KEY` | An Antigravity gateway: bearer auth, agy's gateway headers. `claude-*` models run only here |
| `GEMINI_API_KEY` or `GOOGLE_API_KEY` | The Gemini API, sent as `x-goog-api-key` |
| `GEMINI_BASE_URL` | Another base URL for the Gemini API |
| `NANOCODE_ANTIGRAVITY_EXPLICIT_CACHE=1` | Turns on explicit context caching (off by default) |

Embedders configure the same with `AntigravityHarness::new().endpoint(..).retry(..).explicit_cache(..)` and
`RuntimeBuilder::harness`.

## Crates

| Crate | Owns |
| --- | --- |
| `nanocode-backend-antigravity-client` | The Gemini wire format and its byte-exact encoder, SSE streaming, retries, explicit context caching |
| `nanocode-backend-antigravity-tools` | The engine tools with agy's declarations, validation errors and output formats, background tasks and timers |
| `nanocode-backend-antigravity` | The driver: pinned prompts, request assembly, reminders, messages, compaction, model profiles |

## What it reproduces

- **Prompt.** `prompts/system.md` is agy's system instruction with `<messaging>` and `<subagents>`
  rewritten for `spawn_agent`, `send_agent_message`, `wait_agent`; the user information block takes the workspace,
  OS, app data directory and conversation ID, and `<skills>` lists nanocode's skills the way agy lists its own.
- **First user turn.** `<USER_REQUEST>`, `<ADDITIONAL_METADATA>` with the local time, and on the first turn
  `<USER_SETTINGS_CHANGE>` naming the model.
- **Tools.** `read_file`, `run_command`, `manage_task`, `schedule`, `write_file`, `edit_file`, `search_web`, `ask_question`
  with agy's declarations byte for byte, then nanocode's tools (`exec`/`wait`, subagent tools, MCP) as Gemini
  declarations. Results carry `Created At`/`Completed At`; outputs over 10,240 bytes are saved under
  `brain/<conversation>/.system_generated/steps/<step>/output.txt` and shown head and tail.
  `run_command` backgrounds itself after `WaitMsBeforeAsync`; background results, cancellations and timers come
  back as `<SYSTEM_MESSAGE>` messages, appended to the last tool result or sent as their own user turn when idle.
- **Reminders.** The efficiency reminder after two rounds in a row of at most one effective call, as agy counts them.
- **Requests.** `streamGenerateContent?alt=sse`, agy's labels, headers and generation config per model, its JSON
  spacing, sorted argument keys, echoed thought signatures and its placeholder signature on unsigned calls. The
  system instruction and tools never change during a session, so Gemini's implicit cache keeps the prefix.
- **Compaction.** At a reported prompt size of 512,000 tokens (256,000 for gemini-3 and claude), the same prefix plus
  agy's summary prompt with 32,768 output tokens, then a `<CONTEXT_SUMMARY>` turn with the last ten requests and the
  running tasks. The full transcript stays in `.system_generated/logs/transcript*.jsonl`. `/compact` runs it on demand.

`scripts/antigravity-request-parity.py` replays one model script against `agy` and nanocode through a mock gateway
and diffs their requests.

## Not ported

- agy's subagent, messaging, workflow and swarm tools and its MCP handling: nanocode's own replace them.
- The safety rater that agy runs before commands and writes: nanocode's approval policy decides instead.
