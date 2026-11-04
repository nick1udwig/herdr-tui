# herder-tui

An independent terminal client for Herdr Core.
Shared terminals and workspace layouts remain compatible with herdr-app.
The TUI keeps focus, zoom, selection, and preferences private.

## Build

Use Rust, libclang, and Zig 0.16.
Run `git submodule update --init` to obtain the pinned Ghostty source.
Keep `herdr-client-lib` beside this repository for development.
Set `ZIG` if Zig is not on PATH.
Run `just check` and `just build`.

## Run

Run `target/debug/herder-tui` for the workspace interface on the same core as `hc`.
The session defaults to `HC_SESSION`, or `default` when that variable is unset or empty.
Use `--session SESSION` to select another core session and `--instance NAME` to name the viewer.
The instance name is optional for interactive use and required for live control.
Add a terminal address to use the single-terminal viewer; Ctrl+Q quits it.
Set `HC_BINARY` to select a core executable.
Detaching leaves shared terminal programs running.
A new session opens a workspace in the folder where the TUI starts, as Herdr does.
A session whose workspaces were all closed stays empty.

## Interface

The interface follows Herdr: a sidebar with spaces above and agents below, a tab bar above the panes, and Herdr's menus, prompts, and mode bar.
Spaces show their state dot, name, and Git branch.
Linked worktrees appear under their main checkout; click the arrow to collapse the group.
The agents list shows recognized coding agents, including ones without a pane.
Agents are identified by their OSC 7501 app name or current program, such as Codex, Claude Code, Pi, and OpenCode.
Status reports from ordinary terminals do not add them to the agents list.
Use the pane menu's `Move pane...` chooser to move a pane to a tab in any workspace on the current machine.
Click `grouped` to sort agents by priority instead.
A finish you did not watch stays marked done across restarts until you view it.
Panes have borders only when a tab shows several; the focused pane's border uses the accent color.

## Controls

Herdr's default keymap applies.
Press Ctrl+B, then a key.
Press prefix+? for the full keybind list and prefix+q to detach.

| Key | Action |
|---|---|
| shift+n / shift+w / shift+d | New / rename / close workspace |
| shift+g | New worktree |
| c / shift+t / shift+x | New / rename / close tab |
| n / p / 1..9 | Next / previous / numbered tab |
| v / minus | Split vertical / horizontal |
| h j k l | Focus an adjacent pane |
| shift+h j k l | Swap with an adjacent pane |
| tab / shift+tab | Cycle panes |
| x / shift+p / z | Close / rename / zoom pane |
| r | Resize mode |
| [ | Copy mode |
| w | Workspace navigation |
| g | Go to any space, pane, agent, or terminal without a pane |
| b | Collapse the sidebar |
| s | Settings |
| o | Go to the latest agent notification |
| e | Open the pane's scrollback in `$EDITOR` |

Unset by default, as in Herdr: `clear_pane`, `resize_pane_left/down/up/right`, `switch_workspace`, and `focus_agent`.
Bind digit ranges as `switch_workspace = "prefix+alt+1..9"`, or set `[keys.indexed]` `tabs`, `workspaces`, or `agents` to a modifier such as `alt`.
`navigate_workspace_up/down` and `navigate_pane_left/down/up/right` change the keys inside workspace navigation.
Add custom command keys as Herdr's `[[keys.command]]` tables, each with `key`, `command`, optional `type`, and `description`.
`shell` runs in the background with `HERDR_ACTIVE_WORKSPACE_ID`, `HERDR_ACTIVE_TAB_ID`, `HERDR_ACTIVE_PANE_ID`, `HERDR_ACTIVE_PANE_CWD`, and `HC_SESSION`; `pane` and `popup` open a zoomed pane.
Clear pane drops this viewer's history and the rows above the cursor line; the program and other viewers keep theirs.

At or below `[ui] mobile_width_threshold` columns (64 by default), Herdr's mobile layout replaces the sidebar and tab row with a two-row header.
Tap `switch` or press prefix+w for a full-screen list of machines, agents, spaces, tabs, and the menu.

Click spaces, tabs, and agents to navigate.
Drag spaces and tabs to reorder them; a main checkout moves with its linked worktrees.
Click `new` for a workspace, `menu` for keybinds, reload, and detach, and `+` for a tab.
Right-click a space, tab, or pane for its menu.
Space menus offer worktree creation, opening, and removal in Git repositories.
In the new worktree dialog, select `new branch` or `existing branch` with Tab or the mouse.
Use Up/Down or click to select the branch or checkout path field.
The default path follows the branch name until you edit the path.
Relative checkout paths start from the repository on the target machine.
Set `[worktrees] directory` to change where new checkouts go, as in Herdr.
Removing a checkout with dirty files or unreferenced commits asks once more; `delete anyway` deletes that work.
Pane menus also park the terminal without ending it.
Right-click an agent without a pane to keep it in a new tab.
Shift+right-click sends the click to the program.
Drag pane borders, the sidebar edge, or the line above the agents to resize.
Drag in a pane to select text when the program does not use the mouse; dragging past an edge scrolls history, and release copies the selection.
Double-click selects a word, URL, or path as Herdr does; triple-click selects the line.
Shift+drag selects in programs that use the mouse.
Ctrl+click opens an HTTP or HTTPS link with `xdg-open` on Linux.
The mouse wheel scrolls history when the program does not use mouse reporting.

In copy mode, use h/j/k/l and w/b/e to move, v or Space to select, and y or Enter to copy.
Use PageUp and PageDown for history, / to search retained history, n to repeat, and q or Esc to leave.
Copies go through `wl-copy`, `xclip`, `xsel`, or `pbcopy`, else OSC 52; over SSH, in VS Code, or in WSL, OSC 52 comes first.
Programs inside a pane can also copy text to the host clipboard with OSC 52.
In a terminal on another machine, ctrl+v (`remote_image_paste`) or an empty paste sends the clipboard image there, and a pasted image file path sends that file; its path is pasted in.
Paste through the host terminal's paste command.

Closing a workspace asks for confirmation.
Closing a tab or pane asks only when it would close the workspace.
When a shell exits, its pane closes as soon as core confirms the exit.
The last pane also closes its tab and workspace.
Worktree removal revalidates the checkout before it deletes it.

## Configuration

Settings use Herdr's `config.toml` format in `$XDG_CONFIG_HOME/herder-tui/config.toml`, or `~/.config/herder-tui/config.toml`.
Without that file, the TUI reads Herdr's `config.toml` (`HERDR_CONFIG_PATH`, else `~/.config/herdr/config.toml`).
The first change made in settings copies Herdr's file to the TUI's path; Herdr's file stays unchanged.
Sections Herdr has and the TUI does not use are ignored.
Set `[theme] name` to a Herdr theme such as `catppuccin`, `tokyo-night`, `dracula`, `nord`, or `gruvbox`, with `[theme.custom]` color tokens on top.
Set `[ui] status_indicators` to `dots` or `symbols`, `agent_panel_sort` to `spaces` or `priority`, `sidebar_width`, and `accent`.
Map Herdr `[keys]` action names to one binding or a list, for example `next_agent = "prefix+a"`.
An empty binding disables an action.
These Herdr `[ui]` options apply with Herdr's defaults: `sidebar_min_width`, `sidebar_max_width`, `sidebar_start_collapsed`, `sidebar_collapsed_mode`, `mouse_capture`, `copy_on_select`, `mouse_scroll_lines`, `confirm_close`, `prompt_new_tab_name`, `prompt_new_workspace_name`, `pane_borders`, `pane_outer_borders`, `pane_scrollbars`, `pane_gaps`, `show_agent_labels_on_pane_borders`, `hide_tab_bar_when_single_tab`, `tab_bar_position`, and `window_title`.
`[terminal] default_shell` sets the program new panes run; empty uses core's shell.
Set `[ui.sound] enabled = false` to silence agents, or give `path`, `done_path`, and `request_path` mp3 files to replace Herdr's sounds.
An agent asking a question always sounds; a finished agent sounds unless you are watching its pane.
Set `[ui.toast] delivery` to `herdr` for a card inside the TUI, `terminal` for an OSC 9 or OSC 99 notification, or `system` for `notify-send` or `osascript`; `off` is the default.
`delay_seconds` waits before delivering and drops a finish you saw or a request already answered; `[ui.toast.herdr] position` places the card.
prefix+o opens the card's agent.
Sounds play through `paplay`, `pw-play`, `ffplay`, `mpg123`, or `mpv` on Linux and `afplay` on macOS.
Set `HERDER_TUI_DISABLE_SOUND` to silence them for a session.
Omitted settings use Herdr's defaults.
Use the menu's reload config to apply changes.
View state and sidebar layout use the matching XDG state directory under `herder-tui`.
The TUI does not write the app's private view or preferences.

## Terminal support

The viewer follows core size events and reconnects after transport loss or kept-terminal recovery.
It does not relaunch an agent.
Ghostty handles colors, Unicode, alternate screens, keyboard modes, mouse reporting, and bracketed paste.
Kitty graphics use clipped half-block rendering, including decoded PNG images.
Virtual image placements and pixel-perfect graphics are not supported.
Scrollback is bounded to 10,000 lines and 10 MB per cached terminal; image storage is bounded to 16 MB.
Hidden panes on the current machine stay attached and parse output without drawing, as in Herdr, so showing one again needs no repaint.
Linux is the supported platform for this release.
Native macOS is unvalidated; Windows also needs a native local control transport.

## Checks

`just check` runs fast behavior tests, formatting, and lint.
`just integration` requires `hc`, Git, and Python 3; each test uses an isolated scratch core.
It runs PTY lifecycle checks (`integration_pty.py`) and regressions for group close (`group_close.py`), Git polling (`git_polling.py`), pane moves (`pane_move.py`), and worktree creation (`worktree_create.py`).
The worktree test creates and removes default checkout paths under `~/.herdr/worktrees` with a unique test repository name.
`just workflows-integration` requires `HC_TUI_TEST_ROOT` under `/tmp` for Git and workspace workflows.
`just app-integration` requires an isolated running app fixture, `TUI_APP_TEST_ROOT`, `TUI_APP_TEST_TERMINAL`, and `HC_SESSION`.
Do not point integration tests at live work.
`just remote-integration`, `just geometry-integration`, and `just compatibility-integration` use new scratch cores.
`just cli-integration` verifies headless operations and multiple live instances.
`just discovery-integration` verifies hc session defaults and terminal discovery.
`just chrome-integration` verifies the Herdr chrome and mouse workflows.
`just resize-integration` resizes the host repeatedly while panes stream output.
`just profile` measures one and fifteen output-producing panes.
See [validation](docs/validation.md), [performance](docs/performance.md), and [provenance](docs/provenance.md).

## CLI and packaging

Run `herder-tui --session SESSION do help` for shared command names and arguments.
Pass arguments as one JSON object.
Structural commands work while no TUI is open and require explicit target IDs.
For example, `herder-tui --session work do tab.rename '{"id":"tb123","name":"Tests"}'` changes the shared tab.
A close command first returns its affected terminals; submit again with `confirm: true` only when you intend to end them.
Keep the returned review in `expected` to reject changed confirmation targets.

Run `herder-tui --session SESSION instances` to list live viewers.
Run `herder-tui --session SESSION --instance NAME view state` to inspect one viewer.
Live view commands include `pane.focus`, `tab.focus`, `space.focus`, `terminal.focus`, `agent.focus`, `zoom`, `machine`, and `quit`.
Pane, tab, and space focus take an `id`; terminal and agent focus take a `terminal` address.
The control socket is private to the current user and disappears when the viewer exits.
Each named instance has its own saved view.
Use `hc` for terminal automation and machine management.

Run `just install /chosen/prefix` to build an optimized binary and install it under that prefix's `bin` directory.
This does not replace the installed app or core.
Run `just package /path/herder-tui-source.tar.gz` to export the staged source tree with pinned Ghostty and library sources.
The source bundle needs no sibling checkout and already includes its submodules.
Run `just package-check` to export the staged source, verify its library path, and build and test it in a temporary directory.
`distribution/client-revision` selects the library revision; `SOURCE-REVISIONS` records the bundle inputs.
The development checkout keeps its sibling path dependency.
Nothing is published or pushed by these recipes.
