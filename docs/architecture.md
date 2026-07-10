# Rode architecture

## Product boundary

Rode is an agent workspace, not a code editor and not a model provider. It
orchestrates locally installed coding-agent harnesses using the user's existing
authentication. The target workflow is:

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
        +-- libghostty-vt: VT state, reflow, modes, input encoding, render state
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

- `app`: GPUI views, focus, actions, and the top-level application state.
- `editor`: IME-aware multiline input and its low-level shaped-text element.
- `agent`: provider-neutral turn result plus the first Codex CLI adapter.
- `codex`: persistent app-server JSON-RPC transport, streamed events,
  cancellation, and approval responses.
- `codex_auth`: the Codex app-server account client and managed ChatGPT login.
- `git`: repository snapshots, diffs, and isolated thread worktree lifecycle.
- `persistence`: WAL-mode SQLite projections for projects, threads, provider
  resume IDs, workspaces, branches, and messages.
- `terminal`: one worker-owned Ghostty VT and host PTY per Rode thread, plus a
  thin GPUI adapter that paints Ghostty render snapshots and forwards input.

## Host terminal boundary

Rode does not implement a terminal emulator or shell. Each thread lazily owns
a `libghostty-vt` instance and a real host PTY. `libghostty-vt` is the MIT
terminal engine extracted from Ghostty; it owns escape-sequence parsing, grid
and scrollback state, reflow, terminal modes, cursor state, paste safety, and
key/focus encoding. WezTerm's `portable-pty` owns the process boundary. Rode's
code only translates Ghostty's render snapshot into GPUI text and quads.

The child command is the user's existing `$SHELL` (falling back to `/bin/sh`),
started interactively in the active thread worktree. Rode does not force a
login wrapper or replace the user's startup command, so normal `.zshrc` or
`.bashrc` behavior, prompts, aliases, and CLI tools are preserved. The terminal
inherits the host environment with `TERM=xterm-256color`, true-color support,
and `TERM_PROGRAM=Rode`.

Ghostty's complete `ghostty_surface_*` host API currently accepts macOS/iOS
native views, not a Linux GPUI/Wayland surface. Embedding Ghostty's GTK window
inside GPUI is also not possible through Wayland's surface model. The portable
`libghostty-vt` API is therefore the supported reuse boundary: Ghostty owns
terminal semantics while GPUI remains the sole window and renderer. Pointer
press, multi-click, drag, and release events are passed to Ghostty's selection
gesture state machine, and selected text is formatted by Ghostty before GPUI
writes it to the Wayland clipboard.

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

SQLite stores projects, thread metadata, provider session IDs, message
projections, active-thread selection, worktree paths, and branches. Rode
restores the active conversation and all project thread cards on launch. Raw
provider-event journaling, panel layout, and draft restoration remain to be
added.

For each new isolated thread, Rode now creates:

```text
$XDG_STATE_HOME/rode/worktrees/<repo-id>/<thread-id>/
branch: rode/<thread-id>-<slug>
```

The main checkout remains untouched. Worktree creation is transactional: if
branch creation succeeds but checkout fails, Rode cleans up only the objects it
created and records the failure.

## Delivery sequence

1. Native shell, input, provider discovery, managed Codex login, Codex turns,
   and Git diff.
2. Codex app-server transport with streaming events, cancellation, and approval
   cards. (implemented)
3. SQLite state store and restoration of projects, threads, workspaces, provider
   resume IDs, and messages. (implemented; raw event journal and drafts remain)
4. Per-thread Git worktree creation and lifecycle management. (creation and
   removal core implemented; persisted restoration remains)
5. Per-thread Ghostty VT + host PTY terminal with native grid rendering.
   (implemented, including mouse selection and host clipboard copy/paste; OSC
   clipboard requests remain)
6. Commit/push/PR workflow using the user's `git` and `gh` authentication.
7. Notifications, desktop-file packaging, and Wayland compositor compatibility
   tests.
