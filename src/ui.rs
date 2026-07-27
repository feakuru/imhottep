use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
    Frame,
};

use crate::app::{App, CurrentScreen, EditingField, FocusableField, HeaderField, ResponseViewMode};
use crate::keymap::{Action, KeyContext};

// ── Small style helpers ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusState {
    Focused,
    Unfocused,
}

fn focused_border_style(state: FocusState) -> Style {
    match state {
        FocusState::Focused => Style::default().fg(Color::Cyan),
        FocusState::Unfocused => Style::default(),
    }
}

/// Returns the "selected" list-item style (Yellow + DarkGray bg + Bold).
fn selected_item_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

/// Returns the visual column of the cursor at the end of `text` after word-wrapping
/// to `max_width`.  Mirrors ratatui's `Paragraph` with `Wrap { trim: false }`.
fn wrapped_cursor_column(text: &str, max_width: u16) -> u16 {
    use unicode_width::UnicodeWidthStr;

    if max_width == 0 || text.is_empty() {
        return 0;
    }

    let mut col: u16 = 0;

    for logical in text.split('\n') {
        col = 0;
        let mut is_ws = logical.starts_with(|c: char| c.is_whitespace());
        let mut rest = logical;

        while !rest.is_empty() {
            let end = if is_ws {
                rest.find(|c: char| !c.is_whitespace()).unwrap_or(rest.len())
            } else {
                rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len())
            };
            let token = &rest[..end];
            let tw = token.width() as u16;

            if is_ws {
                if col + tw <= max_width {
                    col += tw;
                }
            } else if tw > max_width {
                col = max_width;
            } else if col + tw <= max_width {
                col += tw;
            } else {
                col = tw;
            }

            rest = &rest[end..];
            is_ws = !is_ws;
        }
    }

    col.min(max_width)
}

// ── Layout / rendering helpers ────────────────────────────────────────────────

/// Controls how `render_scrollable_paragraph` adjusts the scroll offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollMode {
    /// Set scroll to the last visible line (auto-follow new content).
    End,
    /// Clamp scroll so it never exceeds the last visible line.
    Clamp,
}

/// Renders a scrollable, word-wrapped `Paragraph` with an optional vertical
/// scrollbar.
fn render_scrollable_paragraph(
    frame: &mut Frame,
    text: String,
    block: Block,
    style: Style,
    area: Rect,
    scroll: &mut u16,
    mode: ScrollMode,
) {
    let inner = block.inner(area);
    let visible_lines = inner.height;

    let line_count = Paragraph::new(text.as_str())
        .style(style)
        .wrap(Wrap { trim: false })
        .line_count(inner.width) as u16;

    let max_scroll = line_count.saturating_sub(visible_lines);
    match mode {
        ScrollMode::End => *scroll = max_scroll,
        ScrollMode::Clamp => *scroll = (*scroll).min(max_scroll),
    }

    let paragraph = Paragraph::new(text)
        .block(block)
        .style(style)
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0));
    frame.render_widget(paragraph, area);

    if line_count > visible_lines {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll as usize).position(*scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

/// Like `render_scrollable_paragraph` but accepts a pre-built `Text` value
/// (used for ANSI-coloured jq output).
fn render_scrollable_text(
    frame: &mut Frame,
    text: Text,
    block: Block,
    area: Rect,
    scroll: &mut u16,
) {
    let inner = block.inner(area);
    let visible_lines = inner.height;

    // Use a block-less paragraph for line counting so that line_count()
    // returns the pure text row count using the inner (content) width.
    let line_count = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(inner.width) as u16;

    let max_scroll = line_count.saturating_sub(visible_lines);
    *scroll = (*scroll).min(max_scroll);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0));
    frame.render_widget(paragraph, area);

    if line_count > visible_lines {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll as usize).position(*scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

// ── Title helper ──────────────────────────────────────────────────────────────

/// Build a block title for a widget:
/// - When `editing`: appends the editing-mode hints from the keymap.
/// - When `focused`: appends the focused-navigation hints from the keymap.
/// - When neither (unfocused/not editing): appends the focus-jump shortcut for
///   this field (e.g. `"u=edit URL"`) so the user can always see how to jump
///   to any field.
fn widget_title(base: &str, app: &App, field: FocusableField) -> String {
    let is_focused = app.focused_field == field;
    let editing_this_field = match field {
        FocusableField::Url => app.editing_field == Some(EditingField::Url),
        FocusableField::Headers => app.editing_field == Some(EditingField::Headers),
        FocusableField::Body => app.editing_field == Some(EditingField::Body),
        FocusableField::Response => matches!(
            app.editing_field,
            Some(
                EditingField::JsonFilter
                    | EditingField::StreamPrefixRegex
                    | EditingField::StreamSuffixRegex
            )
        ),
        FocusableField::RequestEvents => false,
    };

    if editing_this_field {
        // Show hints for the current editing context — field-specific only
        let editing_field = app.editing_field.unwrap();
        let ctx = KeyContext {
            screen: CurrentScreen::Request,
            editing: Some(editing_field),
            focus: field,
        };
        let bindings = app.keymap.field_bindings_for(&ctx);
        let hint_line = bindings
            .iter()
            .map(|b| format!("{} - {}", b.hint, b.description))
            .collect::<Vec<_>>()
            .join(" | ");
        if hint_line.is_empty() {
            base.to_string()
        } else {
            format!("{base} ({hint_line})")
        }
    } else if is_focused {
        // Show field-specific navigation hints only (not the global tab/q/m/s etc.)
        let ctx = KeyContext {
            screen: CurrentScreen::Request,
            editing: None,
            focus: field,
        };
        let bindings = app.keymap.field_bindings_for(&ctx);
        let hint_line = bindings
            .iter()
            .map(|b| format!("{} - {}", b.hint, b.description))
            .collect::<Vec<_>>()
            .join(" | ");
        if hint_line.is_empty() {
            base.to_string()
        } else {
            format!("{base} ({hint_line})")
        }
    } else {
        // Show focus-jump shortcut (always visible so user knows how to reach the field)
        let shortcuts = app.keymap.focus_shortcut_for_field(field);
        if shortcuts.is_empty() {
            base.to_string()
        } else {
            let parts: Vec<String> = shortcuts
                .iter()
                .map(|(hint, desc)| format!("{hint}={desc}"))
                .collect();
            format!("{base} ({})", parts.join(", "))
        }
    }
}

// ── Top-level entry point ─────────────────────────────────────────────────────

pub fn ui(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(5),
        ])
        .split(frame.area());

    let title_block = Block::default()
        .borders(Borders::TOP)
        .title_top(Line::from("ImHoTTeP").centered())
        .style(Style::default());

    frame.render_widget(title_block, chunks[0]);

    match app.current_screen {
        CurrentScreen::Main => render_main_screen(frame, app, chunks[1]),
        CurrentScreen::Request => render_request_screen(frame, app, chunks[1]),
        CurrentScreen::Exiting => {}
    }

    render_footer(frame, app, chunks[2]);

    if let CurrentScreen::Exiting = app.current_screen {
        render_exit_popup(frame);
    }
}

// ── Screen renderers ──────────────────────────────────────────────────────────

fn render_main_screen(frame: &mut Frame, app: &App, area: Rect) {
    let list_items: Vec<ListItem> = app
        .requests
        .iter()
        .enumerate()
        .map(|(idx, req)| {
            let style = if Some(idx) == app.current_request_index {
                selected_item_style()
            } else {
                Style::default()
            };
            ListItem::new(format!("{} {}", req.method, req.url)).style(style)
        })
        .collect();

    let list = List::new(list_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Requests")
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default()),
    );

    frame.render_widget(list, area);
}

fn render_request_screen(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(request) = app.get_current_request() else {
        let msg = Paragraph::new("No request selected")
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(Color::Red));
        frame.render_widget(msg, area);
        return;
    };

    // Split into left (request) and right (response) panels
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50), // Left side: request fields
            Constraint::Percentage(50), // Right side: response
        ])
        .split(area);

    // Left side: request fields
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Method + URL
            Constraint::Length(8), // Headers
            Constraint::Min(8),    // Body
        ])
        .split(main_chunks[0]);

    // Right side: events + response
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(3), // Response
            Constraint::Min(5),  // Request Events
        ])
        .split(main_chunks[1]);

    // ── Method & URL ──────────────────────────────────────────────────────────
    let is_url_focused = app.focused_field == FocusableField::Url;
    let is_url_editing = app.editing_field == Some(EditingField::Url);

    let method_url_text = if is_url_editing {
        format!("{} {} [EDITING]", request.method, app.input_buffer)
    } else {
        format!("{} {}", request.method, request.url)
    };

    let method_url_style = if is_url_editing {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if is_url_focused {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    let url_title = widget_title("Method & URL", app, FocusableField::Url);
    let method_url_block = Block::default()
        .borders(Borders::ALL)
        .title(url_title)
        .border_style(focused_border_style(if is_url_focused { FocusState::Focused } else { FocusState::Unfocused }));

    if is_url_editing {
        let inner = method_url_block.inner(left_chunks[0]);
        let text_width = method_url_text.len() as u16;
        let max_scroll = text_width.saturating_sub(inner.width);
        let method_prefix = format!("{} ", request.method);
        let cursor_visual = method_prefix.len() as u16 + app.cursor_pos as u16;
        let h_scroll = cursor_visual
            .saturating_sub(inner.width.saturating_sub(1))
            .min(max_scroll);
        let method_url = Paragraph::new(method_url_text)
            .block(method_url_block)
            .style(method_url_style)
            .scroll((0, h_scroll));
        frame.render_widget(method_url, left_chunks[0]);
        let cx = left_chunks[0].left() + 1 + cursor_visual - h_scroll;
        frame.set_cursor_position((cx, left_chunks[0].top() + 1));
    } else {
        let method_url = Paragraph::new(method_url_text)
            .block(method_url_block)
            .style(method_url_style);
        frame.render_widget(method_url, left_chunks[0]);
    }

    // ── Headers ───────────────────────────────────────────────────────────────
    let is_headers_focused = app.focused_field == FocusableField::Headers;
    let is_headers_editing = app.editing_field == Some(EditingField::Headers);

    // Computed inside the editing branch; rendered last so it floats above all other widgets
    let mut deferred_autocomplete: Option<(Rect, Vec<&'static str>)> = None;

    if is_headers_editing {
        let mut lines = vec![];
        for (key, value) in &request.headers {
            lines.push(format!("{}: {}", key, value));
        }
        if app.header_field == HeaderField::Key {
            lines.push(format!("Key: {} [EDITING]", app.header_key_buffer));
        } else {
            lines.push(format!("Key: {}", app.header_key_buffer));
            lines.push(format!("Value: {} [EDITING]", app.header_value_buffer));
        }

        let headers_title = widget_title("Headers", app, FocusableField::Headers);
        let headers_block = Block::default()
            .borders(Borders::ALL)
            .title(headers_title)
            .border_style(Style::default().fg(Color::Cyan));

        let headers_text = lines.join("\n");
        let headers_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let inner = headers_block.inner(left_chunks[1]);
        let visible_lines = inner.height;
        let line_count = Paragraph::new(headers_text.as_str())
            .style(headers_style)
            .wrap(Wrap { trim: false })
            .line_count(inner.width) as u16;
        let max_scroll = line_count.saturating_sub(visible_lines);
        let scroll = app.headers_scroll.min(max_scroll);
        let headers_paragraph = Paragraph::new(headers_text)
            .block(headers_block)
            .style(headers_style)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        frame.render_widget(headers_paragraph, left_chunks[1]);
        // Place cursor at the editing line
        let cursor_col = if app.header_field == HeaderField::Key {
            "Key: ".len() + app.header_key_cursor
        } else {
            "Value: ".len() + app.header_value_cursor
        };
        let cursor_row = inner.top() + (line_count - 1).saturating_sub(scroll);
        if cursor_row <= inner.bottom() {
            frame.set_cursor_position((
                (inner.left() + cursor_col as u16).min(inner.right()),
                cursor_row,
            ));
        }
        if line_count > visible_lines {
            let mut scrollbar_state =
                ScrollbarState::new(max_scroll as usize).position(scroll as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓"));
            frame.render_stateful_widget(scrollbar, left_chunks[1], &mut scrollbar_state);
        }

        if app.header_field == HeaderField::Key {
            let suggestions = app.get_filtered_header_suggestions();
            if !suggestions.is_empty() {
                let area = Rect {
                    x: left_chunks[1].x + 2,
                    y: left_chunks[1].y + (request.headers.len() as u16) + 3,
                    width: 40.min(left_chunks[1].width.saturating_sub(4)),
                    height: suggestions.len().min(8) as u16 + 2,
                };
                if area.y + area.height <= frame.area().height {
                    deferred_autocomplete = Some((area, suggestions));
                }
            }
        }
    } else {
        let header_items: Vec<ListItem> = request
            .headers
            .iter()
            .enumerate()
            .map(|(idx, (k, v))| {
                let style = if idx == app.selected_header_index && is_headers_focused {
                    selected_item_style()
                } else if is_headers_focused {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };
                ListItem::new(format!("{}: {}", k, v)).style(style)
            })
            .collect();

        let headers_title = widget_title("Headers", app, FocusableField::Headers);
        let headers_block = Block::default()
            .borders(Borders::ALL)
            .title(headers_title)
            .border_style(focused_border_style(if is_headers_focused { FocusState::Focused } else { FocusState::Unfocused }));

        frame.render_widget(List::new(header_items).block(headers_block), left_chunks[1]);
    }

    // ── Body ──────────────────────────────────────────────────────────────────
    let is_body_focused = app.focused_field == FocusableField::Body;
    let is_body_editing = app.editing_field == Some(EditingField::Body);

    let body_text = if is_body_editing {
        format!("{} [EDITING - ^S to save, esc to cancel]", app.input_buffer)
    } else {
        request.body.as_deref().unwrap_or("(no body)").to_string()
    };

    let body_style = if is_body_editing {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if is_body_focused {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    let body_title = widget_title("Body", app, FocusableField::Body);
    let body_block = Block::default()
        .borders(Borders::ALL)
        .title(body_title)
        .border_style(focused_border_style(if is_body_focused { FocusState::Focused } else { FocusState::Unfocused }));

    let body_prefix = if is_body_editing {
        Some(body_text[..app.cursor_pos].to_string())
    } else {
        None
    };

    render_scrollable_paragraph(
        frame,
        body_text,
        body_block,
        body_style,
        left_chunks[2],
        &mut app.body_scroll,
        if is_body_editing { ScrollMode::End } else { ScrollMode::Clamp },
    );

    if let Some(ref body_prefix) = body_prefix {
        let content = Block::default()
            .borders(Borders::ALL)
            .inner(left_chunks[2]);
        let inner_width = content.width.max(1);
        // ratatui strips trailing newlines via str::lines(), so "hello\n" has
        // line_count == 1 (not 2). Detect this and add the empty trailing line.
        let ends_with_nl = body_prefix.ends_with('\n');
        let prefix_for_count = if ends_with_nl {
            &body_prefix[..body_prefix.len() - 1]
        } else {
            body_prefix.as_str()
        };
        let cursor_line_count = Paragraph::new(prefix_for_count)
            .wrap(Wrap { trim: false })
            .line_count(inner_width) as u16
            + if ends_with_nl { 1 } else { 0 };
        let visible_lines = content.height;
        app.body_scroll = app.body_scroll
            .min(cursor_line_count.saturating_sub(1))
            .max(cursor_line_count.saturating_sub(visible_lines));
        let cursor_row = content.top() + cursor_line_count.saturating_sub(1) - app.body_scroll;
        let col = if ends_with_nl { 0 } else { wrapped_cursor_column(body_prefix, inner_width) };
        if cursor_row <= content.bottom() {
            frame.set_cursor_position((content.left() + col, cursor_row));
        }
    }

    // ── Request Events ────────────────────────────────────────────────────────
    let is_events_focused = app.focused_field == FocusableField::RequestEvents;
    let is_request_pending = app.current_request_is_pending();

    let events_text = if app.current_request_events().is_empty() {
        "(no events yet)".to_string()
    } else {
        app.current_request_events().join("\n")
    };

    let events_style = if is_request_pending {
        Style::default().fg(Color::Cyan)
    } else if is_events_focused {
        Style::default().fg(Color::Gray).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let events_block = Block::default()
        .borders(Borders::ALL)
        .title("Request Events")
        .border_style(focused_border_style(if is_events_focused { FocusState::Focused } else { FocusState::Unfocused }));

    render_scrollable_paragraph(
        frame,
        events_text,
        events_block,
        events_style,
        right_chunks[1],
        &mut app.events_scroll,
        if is_request_pending { ScrollMode::End } else { ScrollMode::Clamp },
    );

    // ── Response ──────────────────────────────────────────────────────────────
    let is_response_focused = app.focused_field == FocusableField::Response;
    let is_json_mode = app.response_view_mode == ResponseViewMode::Json;
    let is_streamed_json_mode = app.response_view_mode == ResponseViewMode::StreamedJson;
    let is_filter_editing = app.editing_field == Some(EditingField::JsonFilter);
    let is_prefix_editing = app.editing_field == Some(EditingField::StreamPrefixRegex);
    let is_suffix_editing = app.editing_field == Some(EditingField::StreamSuffixRegex);

    // Determine the foreground color from response state, then apply the
    // focus background once rather than repeating it in every branch.
    let response_fg = if app.current_request_is_pending() {
        Some(Color::Cyan)
    } else if let Some(Ok(resp)) = app.current_last_response() {
        Some(if resp.is_success() {
            Color::Green
        } else {
            Color::Red
        })
    } else {
        None
    };

    // Build the three-part response block title:
    //   left  = "Response"
    //   center = view mode label (white when focused so it stays visible)
    //   right  = status code (only when a response is available)
    let status_span = if app.current_request_is_pending() {
        Span::styled("...", Style::default().fg(Color::Cyan))
    } else if let Some(ref result) = app.current_last_response() {
        match result {
            Ok(resp) => {
                let color = if resp.is_success() {
                    Color::Green
                } else {
                    Color::Red
                };
                Span::styled(
                    format!("{} {}", resp.status_code.as_u16(), resp.status_code.canonical_reason().unwrap_or("Unknown")),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )
            }
            Err(_) => Span::styled(
                "ERR",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        }
    } else {
        Span::raw("")
    };

    let view_mode_label = app.response_view_mode.label();
    let view_mode_label_color = if is_response_focused {
        Color::White
    } else {
        Color::DarkGray
    };

    let response_block = Block::default()
        .borders(Borders::ALL)
        .title_top(Line::from("Response").left_aligned())
        .title_top(
            Line::from(Span::styled(
                view_mode_label,
                Style::default().fg(view_mode_label_color),
            ))
            .centered(),
        )
        .title_top(Line::from(status_span).right_aligned())
        .border_style(focused_border_style(if is_response_focused { FocusState::Focused } else { FocusState::Unfocused }));

    if is_json_mode && app.current_last_response().is_some() && !app.current_request_is_pending() {
        // Split the response area: body on top, filter bar at the bottom
        let response_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),    // JSON body
                Constraint::Length(3), // Filter bar
            ])
            .split(right_chunks[0]);

        // Run jq and parse ANSI output
        let jq_output = app.run_jq();
        use ansi_to_tui::IntoText as _;
        let json_text = jq_output
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(jq_output.clone()));

        render_scrollable_text(
            frame,
            json_text,
            response_block,
            response_chunks[0],
            &mut app.response_scroll,
        );

        // Filter bar
        let filter_title = if is_filter_editing {
            let ctx = KeyContext {
                screen: CurrentScreen::Request,
                editing: Some(EditingField::JsonFilter),
                focus: FocusableField::Response,
            };
            let hint_line = app.keymap.format_hint_line(&ctx);
            format!("jq filter ({hint_line})")
        } else if is_response_focused {
            let ctx = KeyContext {
                screen: CurrentScreen::Request,
                editing: None,
                focus: FocusableField::Response,
            };
            let bindings = app.keymap.bindings_for(&ctx);
            let filter_hint: Option<String> = bindings
                .iter()
                .find(|b| b.action == Action::EditJqFilter)
                .map(|b| format!("{} - {}", b.hint, b.description));
            match filter_hint {
                Some(h) => format!("jq filter ({h})"),
                None => "jq filter".to_string(),
            }
        } else {
            "jq filter".to_string()
        };

        let filter_display = if is_filter_editing {
            format!("{} [EDITING]", app.input_buffer)
        } else {
            app.current_jq_filter().to_string()
        };
        let filter_style = if is_filter_editing {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if is_response_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let filter_block = Block::default()
            .borders(Borders::ALL)
            .title(filter_title)
            .border_style(if is_filter_editing {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        if is_filter_editing {
            let inner = filter_block.inner(response_chunks[1]);
            let text_width = filter_display.len() as u16;
            let max_scroll = text_width.saturating_sub(inner.width);
            let cursor_visual = app.cursor_pos as u16;
            let h_scroll = cursor_visual
                .saturating_sub(inner.width.saturating_sub(1))
                .min(max_scroll);
            let filter_paragraph = Paragraph::new(filter_display)
                .block(filter_block)
                .style(filter_style)
                .scroll((0, h_scroll));
            frame.render_widget(filter_paragraph, response_chunks[1]);
            let cx = response_chunks[1].left() + 1 + cursor_visual - h_scroll;
            frame.set_cursor_position((cx, response_chunks[1].top() + 1));
        } else {
            let filter_paragraph = Paragraph::new(filter_display)
                .block(filter_block)
                .style(filter_style);
            frame.render_widget(filter_paragraph, response_chunks[1]);
        }
    } else if is_streamed_json_mode {
        // ── StreamedJson mode ─────────────────────────────────────────────────
        // Split: jq output on top, filter bar (3 fields) at bottom.
        let is_pending = app.current_request_is_pending();
        let response_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),    // jq output
                Constraint::Length(5), // filter bar (3 fields stacked)
            ])
            .split(right_chunks[0]);

        // Render jq output (ANSI-coloured)
        let jq_output = app.current_streamed_jq_output();
        use ansi_to_tui::IntoText as _;
        let jq_text = jq_output
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(jq_output.clone()));

        // Auto-scroll to end while streaming; manual scroll otherwise
        {
            let inner = response_block.inner(response_chunks[0]);
            let visible_lines = inner.height;
            let line_count = Paragraph::new(jq_text.clone())
                .wrap(Wrap { trim: false })
                .line_count(inner.width) as u16;
            let max_scroll = line_count.saturating_sub(visible_lines);
            if is_pending {
                app.response_scroll = max_scroll;
            } else {
                app.response_scroll = app.response_scroll.min(max_scroll);
            }
            let paragraph = Paragraph::new(jq_text)
                .block(response_block)
                .wrap(Wrap { trim: false })
                .scroll((app.response_scroll, 0));
            frame.render_widget(paragraph, response_chunks[0]);
            if line_count > visible_lines {
                let mut scrollbar_state =
                    ScrollbarState::new(max_scroll as usize).position(app.response_scroll as usize);
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(Some("↑"))
                    .end_symbol(Some("↓"));
                frame.render_stateful_widget(scrollbar, response_chunks[0], &mut scrollbar_state);
            }
        }

        // Filter bar: three rows stacked inside a 5-line area (borders + 3 content lines)
        let filter_area = response_chunks[1];
        let filter_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // prefix regex row
                Constraint::Length(1), // suffix regex row
                Constraint::Length(1), // jq filter row
            ])
            .margin(1) // leave room for the outer border
            .split(filter_area);

        // Outer border block for the filter area
        let filter_bar_border = Block::default()
            .borders(Borders::ALL)
            .title(
                if is_response_focused
                    || is_prefix_editing
                    || is_suffix_editing
                    || is_filter_editing
                {
                    "filters (f=jq | p=prefix | x=suffix)"
                } else {
                    "filters"
                },
            )
            .border_style(
                if is_prefix_editing || is_suffix_editing || is_filter_editing {
                    Style::default().fg(Color::Cyan)
                } else if is_response_focused {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            );
        frame.render_widget(filter_bar_border, filter_area);

        fn render_row(
            frame: &mut Frame,
            label: &str,
            value: &str,
            is_editing: bool,
            is_focused: bool,
            area: Rect,
            _h_scroll: u16,
            cursor_pos: Option<usize>,
        ) {
            let display = if is_editing {
                format!("{label}: {value} [EDITING]")
            } else {
                format!("{label}: {value}")
            };
            let style = if is_editing {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            if is_editing {
                let cursor_visual = label.len() as u16 + 2 + cursor_pos.unwrap_or(0) as u16;
                let text_width = display.len() as u16;
                let visible_w = area.width.max(1);
                let max_scroll = text_width.saturating_sub(visible_w);
                let hs = cursor_visual.saturating_sub(visible_w - 1).min(max_scroll);
                let p = Paragraph::new(display).style(style).scroll((0, hs));
                frame.render_widget(p, area);
                let cx = area.left() + cursor_visual - hs;
                frame.set_cursor_position((cx, area.top()));
            } else {
                frame.render_widget(Paragraph::new(display).style(style), area);
            }
        }

        let prefix_val = if is_prefix_editing {
            app.input_buffer.clone()
        } else {
            app.current_stream_prefix_regex().to_string()
        };
        let suffix_val = if is_suffix_editing {
            app.input_buffer.clone()
        } else {
            app.current_stream_suffix_regex().to_string()
        };
        let jq_val = if is_filter_editing {
            app.input_buffer.clone()
        } else {
            app.current_jq_filter().to_string()
        };

        render_row(
            frame,
            "prefix",
            &prefix_val,
            is_prefix_editing,
            is_response_focused,
            filter_rows[0],
            app.filter_h_scroll,
            if is_prefix_editing {
                Some(app.cursor_pos)
            } else {
                None
            },
        );
        render_row(
            frame,
            "suffix",
            &suffix_val,
            is_suffix_editing,
            is_response_focused,
            filter_rows[1],
            app.filter_h_scroll,
            if is_suffix_editing {
                Some(app.cursor_pos)
            } else {
                None
            },
        );
        render_row(
            frame,
            "jq   ",
            &jq_val,
            is_filter_editing,
            is_response_focused,
            filter_rows[2],
            app.filter_h_scroll,
            if is_filter_editing {
                Some(app.cursor_pos)
            } else {
                None
            },
        );
    } else {
        // Plain text mode (or pending / no response)
        let response_text = if app.current_request_is_pending() {
            // Render live streamed content as it arrives. If nothing yet, show a hint.
            let streamed = app.current_streamed_body();
            if streamed.is_empty() {
                "Receiving response...".to_string()
            } else {
                streamed.to_string()
            }
        } else if let Some(ref result) = app.current_last_response() {
            match result {
                Ok(response) => response.body.clone(),
                Err(err) => format!("Error: {}", err),
            }
        } else {
            "No response yet (s=send request)".to_string()
        };

        let response_style = {
            let base = match response_fg {
                Some(c) => Style::default().fg(c),
                None => Style::default(),
            };
            if is_response_focused {
                base.bg(Color::DarkGray)
            } else {
                base
            }
        };

        render_scrollable_paragraph(
            frame,
            response_text,
            response_block,
            response_style,
            right_chunks[0],
        &mut app.response_scroll,
        ScrollMode::Clamp,
    );
    }

    // ── Autocomplete (rendered last so it floats above everything) ────────────
    if let Some((area, suggestions)) = deferred_autocomplete {
        frame.render_widget(ratatui::widgets::Clear, area);
        let items: Vec<ListItem> = suggestions
            .iter()
            .enumerate()
            .map(|(idx, suggestion)| {
                let style = if app.header_autocomplete.as_ref().map_or(false, |ac| idx == ac.selected) {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White).bg(Color::DarkGray)
                };
                ListItem::new(*suggestion).style(style)
            })
            .collect();
        let autocomplete_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::DarkGray));
        frame.render_widget(List::new(items).block(autocomplete_block), area);
    }
}

// ── Footer ────────────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let current_navigation_text = vec![
        match app.current_screen {
            CurrentScreen::Main => {
                Span::styled("Choose a request", Style::default().fg(Color::Green))
            }
            CurrentScreen::Request => {
                Span::styled("Editing request", Style::default().fg(Color::Yellow))
            }
            CurrentScreen::Exiting => Span::styled("Exiting", Style::default().fg(Color::LightRed)),
        },
        Span::styled(" | ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{} requests", app.requests.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    let mode_footer = Paragraph::new(Line::from(current_navigation_text).centered())
        .block(Block::default().borders(Borders::ALL));

    let ctx = app.key_context();
    let key_notes_footer = match app.current_screen {
        CurrentScreen::Main => {
            let hint_line = app.keymap.format_hint_line(&ctx);
            let mut lines = vec![Line::from(hint_line).centered()];
            // Show save status on a second line when present
            if let Some(status) = &app.last_save_status {
                lines.push(Line::from(status.clone()).centered());
            }
            Paragraph::new(lines).style(Style::default().fg(Color::Yellow))
        }
        CurrentScreen::Request => {
            if app.editing_field.is_some() {
                // Single line: editing hints
                let hint_line = app.keymap.format_hint_line(&ctx);
                Paragraph::new(Line::from(hint_line).centered())
                    .style(Style::default().fg(Color::Cyan))
            } else {
                // Three lines:
                //   1. field-navigation globals (tab, shift+tab)
                //   2. current-field-specific hints
                //   3. global request actions (m, s, v, q)
                //
                // We split the hints into groups for readability.
                let all_bindings = app.keymap.bindings_for(&ctx);

                // Line 1: tab/shift+tab navigation
                let nav_hints: String = all_bindings
                    .iter()
                    .filter(|b| {
                        matches!(
                            b.action,
                            crate::keymap::Action::FocusNextField
                                | crate::keymap::Action::FocusPreviousField
                        )
                    })
                    .map(|b| format!("{} - {}", b.hint, b.description))
                    .collect::<Vec<_>>()
                    .join(" | ");

                // Line 2: field-specific scroll/edit/header actions
                let field_hints: String = all_bindings
                    .iter()
                    .filter(|b| {
                        matches!(
                            b.action,
                            crate::keymap::Action::ScrollDown
                                | crate::keymap::Action::ScrollUp
                                | crate::keymap::Action::PageDown
                                | crate::keymap::Action::PageUp
                                | crate::keymap::Action::EditFocusedField
                                | crate::keymap::Action::EditSelectedHeader
                                | crate::keymap::Action::AddHeader
                                | crate::keymap::Action::DeleteHeader
                                | crate::keymap::Action::SelectNextHeader
                                | crate::keymap::Action::SelectPreviousHeader
                                | crate::keymap::Action::EditJqFilter
                                | crate::keymap::Action::EditStreamPrefixRegex
                                | crate::keymap::Action::EditStreamSuffixRegex
                        )
                    })
                    .map(|b| format!("{} - {}", b.hint, b.description))
                    .collect::<Vec<_>>()
                    .join(" | ");

                // Line 3: global request actions
                let global_hints: String = all_bindings
                    .iter()
                    .filter(|b| {
                        matches!(
                            b.action,
                            crate::keymap::Action::ToggleMethod
                                | crate::keymap::Action::SendRequest
                                | crate::keymap::Action::CycleViewMode
                                | crate::keymap::Action::GoBack
                                | crate::keymap::Action::TriggerExit
                        )
                    })
                    .map(|b| format!("{} - {}", b.hint, b.description))
                    .collect::<Vec<_>>()
                    .join(" | ");

                Paragraph::new(vec![
                    Line::from(nav_hints).centered(),
                    Line::from(field_hints).centered(),
                    Line::from(global_hints).centered(),
                ])
                .style(Style::default().fg(Color::Yellow))
            }
        }
        CurrentScreen::Exiting => {
            let hint_line = app.keymap.format_hint_line(&ctx);
            Paragraph::new(Line::from(hint_line).centered())
                .style(Style::default().fg(Color::Yellow))
        }
    }
    .block(Block::default().borders(Borders::ALL));

    let footer_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    frame.render_widget(mode_footer, footer_chunks[0]);
    frame.render_widget(key_notes_footer, footer_chunks[1]);
}

// ── Exit popup ────────────────────────────────────────────────────────────────

fn render_exit_popup(frame: &mut Frame) {
    let popup_block = Block::default()
        .title("Exit app")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::DarkGray));

    let exit_text = Text::styled(
        "\nDo you really want to exit? [y|n]",
        Style::default().fg(Color::Yellow),
    );
    let exit_paragraph = Paragraph::new(exit_text)
        .block(popup_block)
        .centered()
        .wrap(Wrap { trim: false });

    let area = centered_rect(60, 25, frame.area());
    frame.render_widget(exit_paragraph, area);
}

/// Returns a centered sub-rectangle of `r`, `percent_x` wide and
/// `percent_y` tall (both as percentages of `r`).
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    // ── centered_rect ─────────────────────────────────────────────────────────

    #[test]
    fn test_centered_rect_is_within_parent() {
        let parent = Rect::new(0, 0, 100, 50);
        let result = centered_rect(60, 25, parent);
        assert!(result.x >= parent.x);
        assert!(result.y >= parent.y);
        assert!(result.x + result.width <= parent.x + parent.width);
        assert!(result.y + result.height <= parent.y + parent.height);
    }

    #[test]
    fn test_centered_rect_width_is_proportional() {
        let parent = Rect::new(0, 0, 100, 100);
        let result = centered_rect(60, 50, parent);
        // 60% of 100 = 60; ratatui layout may round, allow ±2
        assert!(
            (result.width as i32 - 60).abs() <= 2,
            "width was {}",
            result.width
        );
    }

    #[test]
    fn test_centered_rect_height_is_proportional() {
        let parent = Rect::new(0, 0, 100, 100);
        let result = centered_rect(60, 50, parent);
        assert!(
            (result.height as i32 - 50).abs() <= 2,
            "height was {}",
            result.height
        );
    }

    #[test]
    fn test_centered_rect_is_horizontally_centered() {
        let parent = Rect::new(0, 0, 100, 100);
        let result = centered_rect(60, 50, parent);
        let left_margin = result.x;
        let right_margin = parent.width - result.x - result.width;
        // Margins should be roughly equal (rounding may differ by 1)
        assert!(
            (left_margin as i32 - right_margin as i32).abs() <= 1,
            "left={left_margin} right={right_margin}"
        );
    }

    #[test]
    fn test_centered_rect_full_size() {
        let parent = Rect::new(0, 0, 80, 40);
        let result = centered_rect(100, 100, parent);
        // 100% → entire area (ratatui may leave 0 for the margin percentages)
        assert!(result.width > 0);
        assert!(result.height > 0);
    }

    // ── focused_border_style ──────────────────────────────────────────────────

    #[test]
    fn test_focused_border_style_when_focused_is_cyan() {
        let style = focused_border_style(FocusState::Focused);
        assert_eq!(style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_focused_border_style_when_not_focused_is_default() {
        let style = focused_border_style(FocusState::Unfocused);
        // Default style has no foreground colour set
        assert_eq!(style.fg, None);
    }

    // ── selected_item_style ───────────────────────────────────────────────────

    #[test]
    fn test_selected_item_style_colors() {
        let style = selected_item_style();
        assert_eq!(style.fg, Some(Color::Yellow));
        assert_eq!(style.bg, Some(Color::DarkGray));
    }

    #[test]
    fn test_selected_item_style_is_bold() {
        let style = selected_item_style();
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    // ── wrapped_cursor_column ─────────────────────────────────────────────────

    #[test]
    fn test_wrapped_cursor_column_simple() {
        assert_eq!(wrapped_cursor_column("hello", 10), 5);
        assert_eq!(wrapped_cursor_column("", 10), 0);
        assert_eq!(wrapped_cursor_column("  ", 10), 2);
        assert_eq!(wrapped_cursor_column("  hello", 10), 7);
    }

    #[test]
    fn test_wrapped_cursor_column_overflow() {
        // "hello world" with width 10: "hello" line 1, "world" line 2
        assert_eq!(wrapped_cursor_column("hello world", 10), 5);
        // "hello world foo" with width 10: "hello" line 1, "world foo" line 2
        assert_eq!(wrapped_cursor_column("hello world foo", 10), 9);
        // "hello world foo bar" with width 10: "hello" l1, "world foo" l2 (flush at 9+1≥10), "bar" l3
        assert_eq!(wrapped_cursor_column("hello world foo bar", 10), 3);
    }

    #[test]
    fn test_wrapped_cursor_column_fits_on_one_line() {
        assert_eq!(wrapped_cursor_column("hello world", 80), 11);
        assert_eq!(wrapped_cursor_column("a b c d e f g", 80), 13);
    }

    #[test]
    fn test_wrapped_cursor_column_long_word_exceeds_width() {
        // Words longer than max_width are capped at max_width
        assert_eq!(wrapped_cursor_column("abcdefghijklmnop", 5), 5);
    }

    #[test]
    fn test_wrapped_cursor_column_newlines() {
        assert_eq!(wrapped_cursor_column("hello\nworld", 80), 5);
        assert_eq!(wrapped_cursor_column("hello\nworld\nfoo", 10), 3);
    }
}
