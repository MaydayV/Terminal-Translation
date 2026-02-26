# TerminalTranslation (`tetr`) MVP Implementation Plan

> **For Codex:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a `tetr` CLI tool that launches a wrapped shell and streams translated terminal output into a floating bottom window.

**Architecture:** A CLI main process handles PTY forwarding, output capture, and translation. A Tauri React UI process renders translation results and can request session stop.

**Tech Stack:** Rust (`portable-pty`, `clap`, `reqwest`, `serde`), Tauri v2, React + TypeScript.

## Scope
- macOS + Windows support in MVP.
- `tetr` starts session with child shell.
- Output is forwarded to original terminal and translated to UI.
- `exit` or UI close stops session.
- Long outputs are truncated before translation.
- Mock-based automated tests.

## Public Interfaces
- CLI:
  - `tetr`
  - `tetr --provider <name>`
  - `tetr --no-ui`
- Env:
  - `TETR_PROVIDER`
  - `TETR_DEEPSEEK_API_KEY`
  - `TETR_DEEPSEEK_BASE_URL`
  - `TETR_MODEL`
  - `TETR_TRANSLATE_IDLE_MS`
- IPC events:
  - `session.started`
  - `translation.started`
  - `translation.delta`
  - `translation.done`
  - `session.stopped`
  - `session.error`
- Translator trait:
  - `stream_translate(input, callback_delta) -> Result<TranslationMeta>`

## Milestones
1. Workspace bootstrap and dependencies.
2. Capture state machine + tests.
3. Truncation logic + tests.
4. Translator abstraction + mock + DeepSeek stream parser/tests.
5. PTY bridge implementation + normalization tests.
6. CLI orchestration and lifecycle wiring.
7. Tauri React UI and IPC bridge.
8. Integration checks and smoke scripts.
9. Packaging notes and PATH setup docs.

## Assumptions
- MVP uses direct-run mode (no daemon).
- Interactive full-screen apps are passthrough only.
- Real API calls are not required in automated tests.
