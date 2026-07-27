# imhottep

HTTP(S) TUI client — send HTTP(S) requests, inspect responses, edit headers/body interactively.

## Build / Run / Test

```sh
cargo build
cargo run
cargo test
```

## Architecture

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point, event loop, `execute_action()` dispatcher |
| `src/app.rs` | `App` struct — all state (buffers, cursors, scroll, requests, config) |
| `src/ui.rs` | Ratatui rendering — layout, cursor positioning, scrollable paragraphs |
| `src/keymap.rs` | `Keymap` — context-aware keybinding resolution (`KeyTrigger`), action enum |

## Layout (Request screen)

```
┌─ Method URL ─────────────────────────────────────┐
│ GET https://api.example.com                       │
├─ Headers ─────────────────────────────────────────┤
│ Content-Type: application/json                    │
│ Authorization: Bearer ...                         │
├─ Body ────────────────────────────────────────────┤
│ {"key": "value"}                                  │
├─ Request Events ───────┬─ Response ───────────────┤
│ 2025-01-01 12:00:00    │ 200 OK                   │
│ Sent request           │ {"status": "ok"}         │
└────────────────────────┴──────────────────────────┘
```

## Editing

Six editable fields: `Url`, `Headers`, `Body`, `JsonFilter`, `StreamPrefixRegex`, `StreamSuffixRegex`.

Each has its own cursor position and scroll state:
- `input_buffer` / `cursor_pos` — shared by Url, Body, JsonFilter, prefix, suffix
- `header_key_buffer` / `header_key_cursor` — Headers key field
- `header_value_buffer` / `header_value_cursor` — Headers value field

Scroll state:
- `url_h_scroll` — horizontal scroll for Url
- `filter_h_scroll` — horizontal scroll for JsonFilter, prefix, suffix
- `body_scroll` — vertical scroll for Body
- `headers_scroll` — vertical scroll for Headers

### Cursor column (wrapped text)

Use `wrapped_cursor_column(text, max_width)` for the visual column within the last wrapped line of `text`. It tokenizes by whitespace and simulates ratatui word-wrap to find the cursor's position. Handles `\n` boundaries by resetting column per logical line.

Ratatui strips trailing newlines via `str::lines()`. When `body_prefix` ends with `\n`, add 1 to `line_count` and set `col = 0`.

## Word navigation (CursorWordLeft / CursorWordRight / DeleteWordBackward / DeleteWordForward)

Words are defined by **alphanumeric category boundaries** — the cursor jumps/stops between blocks of alphanumeric characters and blocks of non-alphanumeric characters.

Examples:
- `https://api.example.com/v1/users` traverses as: `https` → `://` → `api` → `.` → `example` → `.` → `com` → `/` → `v1` → `/` → `users`
- `hello, world!` traverses as: `hello` → `,` → ` ` → `world` → `!`

Implementation: detect `chars[i].is_alphanumeric()` on the first non-whitespace char, then skip while `is_alphanumeric() == is_alnum && !is_whitespace()`.

## Keymap

`KeyTrigger` variants:
- `Char(c)` — matches `KeyCode::Char(c)` with `modifiers == NONE`
- `Code(code)` — matches `KeyCode` with `modifiers == NONE`
- `Modified(mods, code)` — matches `KeyCode` with `modifiers.contains(mods)`

Resolution: collect all matching `ContextRule`s sorted by specificity descending, iterate bindings then triggers, return first match.

**Important**: `KeyTrigger::Code` **must** require `modifiers == NONE` — otherwise it intercepts modified events (e.g. `Code(Left)` would catch Ctrl+Left).

### Modifier fallbacks

- Ctrl+Arrow may not work on all terminals → add Alt+Arrow as fallback
- Ctrl+Backspace may decode as `Char('h')` with CONTROL (0x08 = Ctrl+H) → add `Modified(CONTROL, Char('h'))` as trigger
- Alt+Backspace for word-delete fallback
- Ctrl+Delete / Alt+Delete for word-delete-forward

### Key routing (event loop)

In editing mode: chars with NONE/SHIFT modifiers are routed directly to the input buffer (insert at cursor). All other events (Ctrl+key, arrows, function keys, etc.) fall through to `keymap.resolve()`.
