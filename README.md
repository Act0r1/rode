# Rode

Rode is a Linux-first, native Wayland control plane for coding agents. It is
inspired by T3 Code's project/thread workflow, but it is written in Rust and
does not embed a browser, Electron, or Tauri.

The UI is rendered by Zed's GPUI stack pinned to a known revision. On Linux,
that means direct Wayland integration, `xkbcommon` input, and GPU rendering via
`wgpu`/Vulkan.

## Current prototype

- native Wayland window and GPU-rendered three-pane interface;
- project and provider discovery for Codex and Claude Code;
- in-app ChatGPT sign-in through Codex's managed OAuth flow;
- persistent Codex app-server sessions with streamed messages, reasoning,
  commands, file changes, cancellation, and approval cards;
- isolated `rode/<thread>-<slug>` Git worktrees for new threads;
- SQLite-backed projects, threads, provider resume IDs, worktree paths, and
  conversation restoration;
- Git branch, dirty-file count, diff stats, and an in-app diff view;
- Unicode/IME-aware prompt editor using GPUI's platform input path;
- workspace-write sandboxing as the safe default.

The terminal, Claude/ACP adapter, and commit/push/PR actions are described in
[the architecture](docs/architecture.md) and will be layered on the same
provider-neutral core.

## Build

On Arch Linux, install the normal Zed/GPUI Linux build dependencies plus
Wayland and Vulkan development packages. The core requirements are a recent
stable Rust toolchain, `wayland`, `libxkbcommon`, `vulkan-icd-loader`,
`fontconfig`, and a working Vulkan driver.

```sh
cargo run -- /path/to/a/project
```

The project path defaults to the current directory. Rode detects agent CLIs
from `PATH`. If Codex is signed out, use **Sign in with ChatGPT** in the
sidebar; Rode opens the browser flow exposed by `codex app-server` and updates
the account card when Codex confirms the login.

Codex owns the OAuth callback, credential persistence, and token refresh. Rode
only receives the account email and plan needed for status display; it never
reads or stores OpenAI access or refresh tokens.

## Why Rust

Rust is the implementation language rather than Zig because the exact stack we
want to learn from—GPUI's Wayland backend and `wgpu` renderer—is Rust-native.
Rust also has mature libraries for async processes, pseudo-terminals, Git,
SQLite, accessibility, and desktop portals. Choosing Zig would require
rebuilding or wrapping most of that infrastructure before reaching the agent
workflow.
