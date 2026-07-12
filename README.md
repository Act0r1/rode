# Rode

Rode is a Linux-first, native Wayland control plane for Codex. It is
inspired by T3 Code's project/thread workflow, but it is written in Rust and
does not embed a browser, Electron, or Tauri.

The UI is rendered by Zed's GPUI stack pinned to a known revision. On Linux,
that means direct Wayland integration, `xkbcommon` input, and GPU rendering via
`wgpu`/Vulkan.

## Current prototype

- native Wayland window and GPU-rendered three-pane interface;
- installed Codex discovery and a dedicated account-onboarding screen;
- in-app ChatGPT sign-in through Codex's managed OAuth flow;
- persistent Codex app-server sessions with streamed messages, reasoning,
  commands, file changes, cancellation, and approval cards;
- multiple project folders with persistent, renameable project and thread cards;
- project-folder threads by default, with opt-in isolated
  `rode/<thread>-<slug>` Git worktrees for new threads;
- SQLite-backed projects, threads, UI settings, provider resume IDs, worktree
  paths, and conversation restoration;
- Git branch, dirty-file count, structured stack/split diffs, commit, push, and
  pull-request creation through the user's existing `git` and `gh` setup;
- one persistent Ghostty-powered host terminal per thread, opened in that
  thread's workspace and running the user's interactive `$SHELL`;
- desktop notifications when Codex turns finish;
- Unicode/IME-aware prompt editor using GPUI's platform input path;
- workspace-write sandboxing as the safe default.

Rode intentionally targets Codex and does not include Claude or OpenCode
adapters. The implementation boundaries are described in
[the architecture](docs/architecture.md).

## Build

On Arch Linux, install the normal Zed/GPUI Linux build dependencies plus
Wayland and Vulkan development packages. The core requirements are a recent
stable Rust toolchain, `wayland`, `libxkbcommon`, `vulkan-icd-loader`,
`fontconfig`, and a working Vulkan driver. Building the pinned Ghostty VT core
also requires Zig 0.15.x on `PATH` (0.15.2 is tested).

```sh
cargo run -- /path/to/a/project
```

The project path defaults to the current directory. Rode detects `codex` from
`PATH`. When signed out, Rode displays a Codex-only onboarding screen and opens
the managed browser flow exposed by `codex app-server`.

Codex owns the OAuth callback, credential persistence, and token refresh. Rode
only receives the account email and plan needed for status display; it never
reads or stores OpenAI access or refresh tokens.

Use the sidebar **+** menu to start a thread in the active project or add
another folder. New threads share the selected folder by default. Enable
**Settings → Isolated worktree** when you want future threads to get separate
Git worktrees and thread-specific working-tree diffs. Rode remembers this
setting until you turn it off again.

The terminal button (or `Ctrl+J`) opens the active thread's host terminal. Rode
starts the exact shell selected by `$SHELL`; zsh/bash startup configuration,
prompts, aliases, and tools remain intact. Mouse selection uses Ghostty's
selection engine. `Ctrl+Shift+C` and `Ctrl+Shift+V` copy and paste through the
Wayland clipboard.

The diff panel supports Stack and Split layouts (`Ctrl+Shift+D`). Enter a
commit message or PR title below the diff, then use **Commit all**, **Push**, or
**Create PR**. Rode invokes the local `git` and `gh` commands and does not store
separate repository credentials.

To build and install the desktop entry, icon, metadata, and release binary:

```sh
./packaging/install.sh
```

## Why Rust

Rust is the implementation language rather than Zig because the exact stack we
want to learn from—GPUI's Wayland backend and `wgpu` renderer—is Rust-native.
Rust also has mature libraries for async processes, pseudo-terminals, Git,
SQLite, accessibility, and desktop portals. Choosing Zig would require
rebuilding or wrapping most of that infrastructure before reaching the agent
workflow.
