# Validation

Validated on Linux on 2026-10-29 with hc 0.1.14 and unchanged installed herdr-app 0.1.8.
The app ran on a private headless Weston display with scratch app and core state.
The installed app matches the source baseline version; its exact build revision was not verified.
The library's earlier interoperability run also used an unchanged build of pinned app revision `af2bd72c176df7b852df84a026c684dd6586f010`.
Original Herdr, app, core, and Libertty source checkouts were not changed.

| Check | Result |
|---|---|
| Formatting, Clippy, fast behavior tests | Passed: sixteen tests |
| Rust 1.89.0 | All targets checked |
| PTY lifecycle | Repaint, repeated attach, detach, core restart, kept process, host restoration |
| Workspace workflows | Creation, split, resize, swap, stale close rejection, worktree creation/removal |
| Unchanged app | App-created terminal, TUI input, TUI-created tab, mouse rename, split, close |
| Independent views | App focus/zoom unchanged; two TUI instances retain separate view state |
| CLI | Headless shared operations, explicit live instance targeting, endpoint cleanup |
| Session discovery | Default, inherited, and explicit sessions; ordinary live terminals; private unplaced-terminal preview |
| Mouse controls | Empty-state New/Menu, space and tab creation, both pane split buttons, tab overflow, narrow layouts |
| Host errors | Attach failure and injected save error restore terminal settings |
| Agent reports | Synthetic OSC 7501 status and keyboard navigation without a pane |
| Unknown content | Mod placeholder and lossless unrelated edits preserve opaque properties |
| Completed terminal | Pane closes; core retains output; no repeated attach or redraw loop |
| Geometry | Two different host sizes, input transfers ownership, no idle resize contention |
| Remote machine | Unavailable layout, rejected offline mutation, refreshed offline edit, same process after reconnect |
| Performance | One and fifteen populated panes; idle, visible output, hidden output |
| Independent package | Vendored library and Ghostty; optimized build outside checkout; scratch-prefix install |
| Native macOS and Windows | Not run; not advertised as supported |

Terminal tests cover Unicode width, colors, cursor, alternate screen, resize, application cursor keys, bracketed paste, SGR mouse, control keys, and Kitty release events.
UI tests cover weighted layouts, stack selection, grapheme editing, retained-history search, frozen selection, wide cells, hyperlinks, and Kitty image clipping.
The final event-stream fix handles multiple notifications per frame and notifications split across frames.
Its library regression test also covers cancellation and exact credit return.

The pinned library tests cover CAS conflicts, spawn/save reconciliation, lost-kept-shell recovery, partial Git/terminal/layout outcomes, changed removal preconditions, remote Git, and bounded slow consumers.
The real-core library workflow suite was rerun after the notification decoder change.
See the library's `docs/validation.md` for those checks.

Remote availability is polled every two seconds when remote documents are watched.
Core does not send a document event for a transport disconnect.
A disconnected machine's layout becomes unavailable; cached terminal records are not treated as exited.
Private views use stable core identities and separate instance state files.

See [performance results](performance.md) for measurement scope, bounded retention, and raw results.
See [the Libertty integration map](libertty.md) for future consumer work.

## Herdr interface port

Rechecked on Linux on 2026-10-30 against installed herdr 0.9.5 and hc 0.1.14.
Side-by-side captures compared the sidebar, agents panel, tab bar, pane borders, mode bars, menus, prompts, keybind help, navigator, and worktree dialogs.

| Check | Result |
|---|---|
| Formatting, Clippy, fast behavior tests | Passed: twenty-eight tests |
| `chrome.py` | Passed: sidebar, new/menu, tab prompt, context menus, tab overflow, narrow host, collapse, detach |
| `discovery.py` | Passed: unplaced terminal reached through Go to |
| `geometry.py` | Passed with Herdr's layout and scrollbar gutter |
| `integration_pty.py`, `cli.py`, `compatibility.py`, `remote.py` | Passed |
| Worktrees in a scratch repository | Created, grouped, collapsed, opened, and removed through the interface |
| `workspace_app.py`, `agents_app.py`, `app_terminal.py` | Updated for Herdr keys; not rerun because no headless Weston display was available |
| `resize.py` | Passed: 400 host resizes while six panes stream; failed before the frame-size fix |
| Drag reorder | Spaces, worktree groups, and tabs reorder in the shared document (`chrome.py`, unit test) |
| Sounds | A fake player logged request for a question, nothing for a watched finish, and done for an unwatched finish |
| Rust 1.89.0 | All targets checked |
| Performance profile | Idle and hidden output: no CPU or renders at 1 and 15 panes; visible output 13.4% and 14.6% CPU, about 1 MiB more memory than before |

## Herdr parity work

Rechecked on Linux on 2026-10-30 with hc 0.1.14 and unchanged installed herdr-app 0.1.10.

| Check | Result |
|---|---|
| Formatting, Clippy, fast behavior tests | Passed: fifty-nine tests |
| `just cross-check` | Clippy passed for `x86_64-pc-windows-gnu` and `aarch64-apple-darwin` from Linux |
| `just integration` | Passed, including startup, keys, settings, Herdr config, UI options, notifications, mobile, selection, attention, and worktree removal |
| `cli`, `discovery`, `chrome`, `compatibility`, `geometry`, `resize`, `remote` | Passed |
| `just app-integration display=…` | Passed: `app_terminal.py`, `workspace_app.py`, `agents_app.py` on the desktop's Wayland socket with scratch app and core state |

No headless compositor was installed; the app checks drew on the person's Hyprland session with their consent.
Windows and macOS were compile-checked only; their runtime behavior is unvalidated.
Windows checks use the MinGW target because Zig cannot supply MSVC's C library on Linux.

## Limits

The local control endpoint uses Unix sockets, and named pipes for the current user on Windows.
Native macOS and Windows behavior is unvalidated.
Clipboard writes use native helpers when present, else host OSC 52 support.
Kitty graphics use clipped half-block cells; virtual placements and pixel-perfect rendering are unsupported.
Completed terminal output remains readable through core after its pane closes.
Host restoration is guaranteed for handled exits and panics, not SIGKILL or a host terminal crash.

## Shell exit cleanup

Rechecked on Linux on 2026-11-02 with hc 0.1.17.
Before the fix, Ctrl+D finished the shell but left its split in the shared layout.
The isolated `pane_exit.py` test now closes that split in 11 ms.
It also covers output from a neighboring pane, focus fallback, hidden tabs, kept-shell recovery, final workspace closure, startup cleanup, and host restoration.

Both repositories pass `just check`; the TUI has 59 passing fast tests.
The TUI checks used an isolated copy to exclude concurrent changes in `src/vt.rs`.
The library real-core suite and TUI PTY lifecycle, startup, compatibility, remote, geometry, and CLI checks pass.
Finished panes close through version-checked library edits, with retries after conflicts.
Missing records, lost live terminals, and unavailable machine documents do not trigger closure.

## Host terminal colors

Rechecked on Linux on 2026-11-04 with hc 0.1.17.
ANSI palette indices and default colors now reach the host terminal unchanged
in both workspace and single-terminal views. Truecolor stays RGB, and inverse
uses the host's reverse-video attribute.

The regression failed against the original installed binary: ANSI red index 1
became fixed RGB(204,102,102). The corrected binary passes `tests/colors.py`
against an isolated core in both views, including input, detach, terminal
settings restoration, and surviving core terminals.

Formatting passes. Release unit tests pass with 67 passed and one existing
ignored test. CLI, compatibility, remote, and geometry integration checks pass.
Strict Clippy remains blocked by existing warnings in unchanged
`src/shell/keys.rs` (`nonminimal_bool`) and `src/workspace/interaction.rs`
(`collapsible_match`).

Final review found two minor limits, deferred:

- An explicit palette or default-color override equal to Ghostty's initial
  RGB value is indistinguishable from an unchanged value. It inherits the
  host color instead of preserving that explicit RGB override.
- Transparent images are composited against Ghostty's internal palette.
  When the host palette differs, transparent regions over indexed backgrounds
  can differ from the surrounding background.

### Background queries

Rechecked on Linux on 2026-11-04 with hc 0.1.17 and codex-cli 0.166.0.
The viewer now probes the host's default foreground and background before
starting its event reader, using `terminal-colorsaurus`. The probe has a
500 ms timeout. Headless commands do not probe, and an unsuccessful probe
leaves the display facts unspecified. Both views send the detected colors
through the existing attach `display` field, including after reconnect.

`tests/colors.py` covers OSC 10/11 queries and derived prompt backgrounds
against an isolated core, with black, light, and nonresponding hosts in
both views. The previous binary reports Ghostty's `#282c34` background
and renders the dark prompt as RGB(65,69,76). With a black host, the new
binary reports black and renders RGB(30,30,30). Foreground, detach, retained
terminal processes, and host settings restoration are also checked.
Both views also receive SIGINT while the host probe is waiting; they exit
through the registered interrupt handler and restore the original settings.

A real Codex process was started in an isolated core after the workspace
viewer attached to a simulated black host. Its prompt emitted RGB(30,30,30),
matching the official Herdr capture. No prompt was submitted to the model.
Release unit tests still pass with 67 passed and one existing ignored test.
The same two preexisting strict Clippy warnings remain.
CLI, compatibility, remote, and geometry integration checks pass.

Host colors are detected at viewer startup. Codex can cache startup colors,
so reopen an existing Codex process after restarting the corrected viewer.
Final review also confirmed a minor dependency limit: if a host color reply
is split immediately after ESC, `terminal-colorsaurus` can mistake the reply
for an unsupported query and leave display facts unspecified.
