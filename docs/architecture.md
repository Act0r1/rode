# Rode architecture

## Product boundary

Rode is a Codex workspace, not a code editor or model provider. It orchestrates
the locally installed Codex CLI using the user's existing authentication. The
target workflow is:

1. Open one or more Git projects.
2. Start parallel agent threads, isolated in branches/worktrees when desired.
3. Stream agent messages, reasoning summaries, commands, patches, and approval
   requests into one native UI.
4. Inspect the working tree and diff while a turn is running.
5. Open a real PTY terminal in the same thread workspace.
6. Commit, push, and create a pull request after reviewing the changes.

This matches the durable parts of T3 Code while making Linux/Wayland a primary
platform rather than a packaged web runtime.

## Native platform stack

```text
Rode views and state
        |
        v
GPUI elements / layout / text / accessibility
        |
        +-- gpui_linux: Wayland protocols, clipboard, xkbcommon, portals
        +-- gpui_wgpu: scene batching, glyph atlas, WGSL pipelines
        +-- calloop: compositor and async event-loop integration
        +-- libghostty-vt: VT parsing, grid, reflow, modes, and input encoding
        +-- portable-pty: host shell process and resize/signalling boundary
        |
        v
Wayland compositor + Vulkan/other wgpu backend
```

The GPUI dependencies are pinned to Zed commit
`b2db24e58a085de575a875c151963c98e1a60bec`. Only the `wayland` feature is
enabled; Rode does not silently fall back through XWayland.

Zed's Linux code informed several deliberate choices:

- native `wayland-client` surfaces and protocol negotiation;
- `xkbcommon` and the compositor text-input path instead of interpreting raw
  ASCII key events;
- a retained scene with batched GPU primitives and a glyph atlas;
- surface reconfiguration on compositor resize and device-loss recovery;
- XDG desktop portals for file dialogs, URL opening, and secrets where
  appropriate.

## Modules

- `main`: Linux application startup, dependency wiring, and window creation.
- `app`: top-level route/modal state, async coordination, and the existing GPUI
  workspace rendering while views are extracted incrementally.
- `actions`: the typed GPUI actions, default keybindings, and shared action
  metadata registry used by future menus and the command palette.
- `theme`: semantic color, status, diff, shadow, and corner-radius tokens.
- `ui`: reusable native buttons, modal frame, tabs, toast queue, and selectable
  rows, plus viewport-aware resizable split panes.
- `editor`: IME-aware multiline input and its low-level shaped-text element.
- `agent`: installed Codex discovery.
- `codex`: persistent app-server JSON-RPC transport, streamed events,
  cancellation, and approval responses.
- `codex_auth`: the Codex app-server account client and managed ChatGPT login.
- `diff`: unified-diff parsing plus stack/split row models.
- `git`: repository snapshots, commits, pushes, PR creation, and isolated
  thread worktree lifecycle.
- `persistence`: WAL-mode SQLite projections for projects, threads, provider
  resume IDs, workspaces, branches, and messages.
- `terminal`: a worker-owned Ghostty VT and host PTY per Rode thread, plus the
  GPUI render/input adapter.
- `notifications`: best-effort freedesktop turn-completion notifications.

Codex authentication already uses the
[`codex app-server`](https://developers.openai.com/codex/app-server) JSON-RPC
account surface. Rode calls `account/read`, starts managed browser OAuth with
`account/login/start`, opens the returned HTTPS URL, and waits for
`account/login/completed`. Codex owns the callback listener, token storage, and
refresh lifecycle; Rode does not handle tokens.

Agent turns use a persistent app-server transport. Rode initializes the child,
opens or resumes a provider thread, starts turns, and consumes message,
reasoning, command, file-change, and completion notifications as they arrive.
Command and file-change server requests become native approval cards; the user
can approve or decline without falling back to a terminal. Running turns can be
interrupted with `turn/interrupt`.

Rode deliberately targets Codex only. Claude and OpenCode adapters are outside
the product scope.

## Host terminal boundary

Rode does not implement a terminal emulator or replace the user's shell. Each
thread lazily owns a `libghostty-vt` instance and a real host PTY.
`libghostty-vt` owns escape-sequence parsing, scrollback, reflow, modes, cursor
state, selection, paste safety, and key/focus encoding. Rode only translates
Ghostty render snapshots into GPUI text and quads.

The child is the user's existing `$SHELL` (falling back to `/bin/sh`) started in
the active thread workspace. Ghostty's full surface API cannot currently host a
Linux GPUI surface, so its portable VT library is the correct reuse boundary.

## Safety model

Rode does not equate a polished GUI with blanket command authorization.
Per-thread runtime modes map to provider-native policies:

- `Read only`: no workspace writes.
- `Workspace write` (default): writes stay in the selected project/worktree;
  network or broader access remains denied unless the provider supports a
  surfaced approval.
- `Full access`: explicit opt-in, visibly marked, never inferred from a previous
  thread.

The Codex transport uses `approvalPolicy = "on-request"` with the
`workspace-write` sandbox. Requests for commands or writes outside that policy
are surfaced in the conversation and Rode never auto-upgrades them to full
access.

## Persistence and isolation

SQLite stores projects, custom project and thread names, thread metadata,
provider session IDs, message projections, active-thread selection, worktree
paths, branches, and the default isolation preference. Rode restores the active
conversation and all project thread cards on launch. Draft text and raw event
journaling are intentionally not treated as durable conversation state.

New threads use the selected project folder by default. When the user enables
isolated worktrees in Settings, each future thread is created at:

```text
$XDG_STATE_HOME/rode/worktrees/<repo-id>/<thread-id>/
branch: rode/<thread-id>-<slug>
```

The main checkout remains untouched. Worktree creation is transactional: if
branch creation succeeds but checkout fails, Rode cleans up only the objects it
created and records the failure.

## Implemented delivery

- Architecture milestone 0: application state/rendering extracted from startup,
  with explicit route and modal state, a shared action registry, theme tokens,
  and reusable native UI primitives. (implemented)
- Shell milestone 1: authenticated application rail and Workspace/Source
  Control/Terminal/Settings routing, Ember/Graphite/Daylight themes, responsive
  split panes, and persisted route/theme/panel widths. (implemented)
1. Native shell, input, provider discovery, managed Codex login, Codex turns,
   and structured Git diff. (implemented)
2. Codex app-server transport with streaming events, cancellation, and approval
   cards. (implemented)
3. SQLite state store and restoration of projects, threads, workspaces, provider
   resume IDs, names, UI settings, and messages. (implemented)
4. Per-thread Git worktree creation and lifecycle management. (creation and
   removal core implemented; worktree paths restore from SQLite)
5. Per-thread Ghostty VT + host PTY terminal with native grid rendering,
   selection, and clipboard support. (implemented)
6. Commit/push/PR workflow using the user's `git` and `gh` authentication.
   (implemented)
7. Turn notifications and desktop-file/icon/metainfo packaging. (implemented)
