# drop-term

A drop-down terminal applet for the COSMIC desktop, inspired by [`Yakuake`](https://github.com/KDE/yakuake)  and [Terminator](https://github.com/gnome-terminator/terminator).

Built on [`alacritty_terminal`](https://docs.rs/alacritty_terminal) for the
terminal emulation core, rendered with a custom canvas in
[libcosmic](https://github.com/pop-os/libcosmic).

## Features

- Drop-down terminal anchored to the top of the screen, toggled from the panel icon
- Preferences panel (gear icon, left of the first tab) with a click-to-pick
  color chart for the `user@host` and current-directory portions of the
  shell prompt.
- Toggle the applet from anywhere on the desktop via an OS-level keyboard
  shortcut (see below)

## Installing

Requires a Rust toolchain and [`just`](https://github.com/casey/just).

```sh
just install
```

Installs the binary to `~/.local/bin` and the desktop entry to
`~/.local/share/applications` (override the prefix with `PREFIX=...`).
Add the applet to your panel from COSMIC Settings once installed.

```sh
just uninstall
```

## System-wide keyboard shortcut

1. Open **COSMIC Settings → Keyboard → Custom Shortcuts**.
2. Add a shortcut with command `pkill -SIGUSR1 -x drop-term` and whatever
   key binding you'd like.

## Keybindings

Standard shell/readline shortcuts (`Ctrl+A`/`Ctrl+E`, `Ctrl+C`, `Ctrl+R`,
`Alt+F`/`Alt+B`, arrow-key history, `Tab` completion, etc.) all work
normally and are handled by your shell, not listed here. The applet adds
the following on top:

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+O` | Split pane horizontally |
| `Ctrl+Shift+E` | Split pane vertically |
| `Ctrl+Shift+Q` | Close active pane |
| `Ctrl+Shift+Z` | Next pane |
| `Ctrl+Shift+X` | Previous pane |
| `Ctrl+Shift+T` | New tab |
| `Ctrl+Shift+W` | Close tab |
| `Ctrl+Tab` | Next tab |
| `Ctrl+Shift+Tab` | Previous tab |
| `Ctrl+Shift+P` | Toggle pin |
| `Ctrl+Shift+C` | Copy selection |
| `Ctrl+Shift+V` | Paste |
| `F12` | Hide the dropdown (only while it already has focus) |

This same table is also shown inside the Preferences panel (gear icon in
the tab bar).
