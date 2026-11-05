# Shotik: instructions for coding agents

Shotik is an open-source Windows screenshot tool with a built-in MCP server, written in Rust
(`crates/`). The Electron version it replaced stays readable at tag `electron-final`: comments and docs
that mention `src/...` point there. When a behaviour is unclear, read that code, do not guess.

Read first: `docs/rust/ARCHITECTURE.md`, `docs/rust/CONTRACT.md`.

## Hard rules

1. **Never touch the user's desktop.** The user works at this machine while agents run.
   - No visible windows, no focus stealing, no global hotkey registration, no real clicks or
     keystrokes. Test instances run in ghost mode (`SHOTIK_GHOST=1`, windows at x+20000).
   - The clipboard is shared with the user: save and restore it around anything that writes to it.
   - Real screen pixels and real audio are private: never commit them, never write them outside a
     temporary directory, and delete that directory when done. Use `test/fake-screen.png` by default.
   - Never kill processes you did not start. `Shotik.exe` on this machine is the user's real app.
   - Never start the binary without `--test` and a private `APPDATA`: without them it opens the user's real
     profile, and it has no `--version` to stop it.
   - The one exception is `test/live/`, which takes over the desktop on purpose: only with the owner's
     agreement and on an idle machine. Ghost mode cannot see focus, DPI changes or native dialogs, so a
     change to any of those needs that check, or a plain statement that it did not get one.
2. **Stay inside your allowed paths.** Each task names the files or crates it owns. If you need a
   change elsewhere, do not make it: describe it in your final summary.
   `crates/shotik-core/src/ports.rs` is shared: additive changes only, and name them in your summary.
3. **Done means verified.** Report the exact command and its result. If something could not be
   verified, say so and why. Do not write "should work".
4. **Explicit errors.** A silent fallback that carries on as if nothing happened is a bug. Every
   port returns a typed error; the caller decides what the user sees.
5. **Test the contract, not the implementation.** Pure logic gets unit tests that survive a refactor.
6. **No secrets in code, docs, commits or logs.** `settings.json` may contain a DeepL key.

## Build environment

Dependencies are compiled once and shared; build output lives on the `E:` drive because `C:` is
short of space. Before any cargo command, in the same shell:

```sh
export CARGO_TARGET_DIR="E:/shotik-targets/<your-task-name>"
export RUSTC_WRAPPER="E:/shotik-targets/bin/sccache.exe" SCCACHE_DIR="E:/shotik-targets/sccache" SCCACHE_CACHE_SIZE=30G
export CARGO_BUILD_JOBS=4
```

Several agents build at once on a machine the user is working or playing on, with little free memory:
keep `CARGO_BUILD_JOBS=4`, prefer `cargo check` and `cargo test -p <crate>` over whole-workspace builds,
and send long build output to a file.

Use Context7 or docs.rs for library APIs instead of memory: versions here are newer than most
training data. Popular, maintained crates may be added to your own crate without asking; name them
in your summary. Never add a GPL or AGPL dependency: the project is MIT.

## Commits

Conventional style in English, as in the existing history: `type(scope): summary`, at most 72
characters. The body says what the problem was, what changed in substance, how it was verified and
what remains uncovered; do not retell the diff. Look through the diff for keys, tokens and dumps
before `git add`. Forbidden: `--no-verify`, force push, amending pushed commits, `reset --hard`,
`clean -fd`.
