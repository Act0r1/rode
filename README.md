# Rode

Rode is a Linux-first, native Wayland control plane for coding agents. It is
inspired by T3 Code's project/thread workflow, but it is written in Rust and
does not embed a browser, Electron, or Tauri.

The UI is rendered by Zed's GPUI stack pinned to a known revision. On Linux,
that means direct Wayland integration, `xkbcommon` input, and GPU rendering via
`wgpu`/Vulkan.

## Current prototype

- native Wayland window and GPU-rendered three-pane interface;
- installed Codex discovery and account status;
- in-app ChatGPT sign-in through Codex's managed OAuth flow;
- persistent Codex app-server sessions with streamed messages, reasoning,
  commands, file changes, cancellation, and approval cards;
- isolated `rode/<thread>-<slug>` Git worktrees for new threads;
- SQLite-backed projects, threads, provider resume IDs, worktree paths, and
  conversation restoration;
- Git branch, dirty-file count, diff stats, and an in-app diff view;
- one persistent Ghostty-powered host terminal per thread, opened in that
  thread's worktree and running the user's interactive `$SHELL`;
- Unicode/IME-aware prompt editor using GPUI's platform input path;
- workspace-write sandboxing as the safe default.

The commit/push/PR workflow is described in
[the architecture](docs/architecture.md). Rode intentionally targets Codex and
does not include Claude or OpenCode adapters.

## Build

On Arch Linux, install the normal Zed/GPUI Linux build dependencies plus
Wayland and Vulkan development packages. The core requirements are a recent
stable Rust toolchain, `wayland`, `libxkbcommon`, `vulkan-icd-loader`,
`fontconfig`, and a working Vulkan driver. Building the pinned Ghostty VT core
also requires Zig 0.15.x on `PATH` (0.15.2 is the tested version).

```sh
cargo run -- /path/to/a/project
```

The project path defaults to the current directory. Rode detects the Codex CLI
from `PATH`. If Codex is signed out, use **Sign in with ChatGPT** in the
sidebar; Rode opens the browser flow exposed by `codex app-server` and updates
the account card when Codex confirms the login.

Codex owns the OAuth callback, credential persistence, and token refresh. Rode
only receives the account email and plan needed for status display; it never
reads or stores OpenAI access or refresh tokens.

The terminal button (or `Ctrl+J`) opens the current thread's terminal. Rode
starts the exact shell selected by `$SHELL` without replacing it with a custom
command or forced login wrapper. Because it is attached to a real PTY, zsh and
bash start interactively and load the user's ordinary shell configuration,
prompt, aliases, and tools. Closing the panel does not stop the shell.
Mouse dragging selects terminal text using Ghostty's own selection rules;
double-click selects words, triple-click selects lines, and `Ctrl+Shift+C` /
`Ctrl+Shift+V` copy and paste through the Wayland clipboard. Hold Alt while
dragging for a rectangular selection.

## Why Rust

Rust is the implementation language rather than Zig because the exact stack we
want to learn from—GPUI's Wayland backend and `wgpu` renderer—is Rust-native.
Rust also has mature libraries for async processes, pseudo-terminals, Git,
SQLite, accessibility, and desktop portals. Choosing Zig would require
rebuilding or wrapping most of that infrastructure before reaching the agent
workflow.
