# nanocode-backend-kimi

A nanocode harness that reproduces the engine of Kimi Code CLI (`@moonshot-ai/kimi-code` 2.7.3) for Kimi models: its
system prompt, its reminders, its repeated-call guard, its compaction and its Chat Completions request shape. Kimi Code's
own tools (Bash, Read, Edit, …) are not part of it yet: the model gets nanocode's tools, in Code Mode or directly
(`KimiHarness::direct_tools`), as Kimi Code calls tools. Subagents, Code Mode and MCP stay nanocode's own.

## Selecting it

```rust
use nanocode::kimi::{KimiBuilder, KimiHarness};

let runtime = nanocode::Nanocode::builder()
    .kimi(KimiHarness::new())
    .harness_model(KimiHarness::selection("k3"))
    .build_async()
    .await?;
```

`KimiHarness::new()` posts to Kimi Code's API (`https://api.kimi.com/coding/v1`) with `KIMI_API_KEY`;
`KimiHarness::endpoint` sets another base URL, key and user agent, and `KimiHarness::sender` routes requests through a
host (on wasm, it is required). The model's context window comes from the endpoint's `/models` catalog
(`context_length`) unless `KimiHarness::context_window` gives it. Thinking maps to K3's efforts: `low`, `high` (the
default) or `max`; K3 always reasons, so other levels are refused.

## What it reproduces

| Kimi Code | Here |
| --- | --- |
| System prompt (`system.md`, `coding.md`, `delivering-work.md`, `project-information.md`) | Pinned in `prompts/` with their source revision and hashes in `prompts/upstream.json`; host instructions fill `${role_additional}` |
| Environment: OS, shell, working directory, its two-level listing, `AGENTS.md` (`~/.kimi-code`, `~/.agents`, then the project from its git root to the working directory) | The same, or what the host passes in `Environment` |
| Request: `thinking: {type: enabled, effort, keep: all}`, `max_completion_tokens` = window − estimated request, `prompt_cache_key` = session, `stream_options.include_usage` | The same |
| Tool schemas: local `$ref`s inlined, a `type` inferred for every child schema from its `enum`, `const` or structure | The same (`normalize_tool_schema`) |
| History: assistant `reasoning_content` always replayed, no `content` beside tool calls when it is blank, tool call IDs sanitized and made unique | The same |
| Tool results: `<system>` markers for failed and empty outputs | The same; nanocode's tools get no wall-time header, as Kimi Code's non-MCP external tools |
| Reminders: the date at the start and when it changes, auto permission mode, both again after a compaction | The same |
| Repeated-call guard: reminders on the 3rd, 5th and 8th identical call in a row, a separate one at the 12th, the turn stopped after the 13th | The same |
| Compaction at 85% of the window (or within 50k tokens of it): the whole history plus the pinned instruction, then the user's messages (20k tokens, head and tail), the summary and a continuation reminder | The same, without Kimi Code's pointer to its on-disk event log |
| Retries: 10 attempts, 0.5 s doubling to 32 s with 25% jitter, `Retry-After`, on timeouts, dropped connections and streams, 408/409/429/5xx/529 except quota exhaustion | The same |
