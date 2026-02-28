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

use crate::app::{App, CurrentScreen, EditingField, FocusableField, ResponseViewMode};

// ── Small style helpers ───────────────────────────────────────────────────────

/// Returns a cyan border style when `focused`, otherwise the default.
fn focused_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

/// Returns the "selected" list-item style (Yellow + DarkGray bg + Bold).
fn selected_item_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

// ── Layout / rendering helpers ────────────────────────────────────────────────

/// Counts how many terminal rows `text` will occupy when soft-wrapped to
/// `inner_width` columns.
fn count_wrapped_lines(text: &str, inner_width: usize) -> u16 {
    text.lines()
        .map(|line| {
            let len = line.len();
            if len == 0 {
                1u16
            } else {
                ((len + inner_width - 1) / inner_width) as u16
            }
        })
        .sum()
}

/// Renders a scrollable, word-wrapped `Paragraph` with an optional vertical
/// scrollbar.  Clamps `scroll` so it never exceeds the last visible line.
fn render_scrollable_paragraph(
    frame: &mut Frame,
    text: String,
    block: Block,
    style: Style,
    area: Rect,
    scroll: &mut u16,
) {
    let inner = block.inner(area);
    let visible_lines = inner.height;
    let inner_width = inner.width.max(1) as usize;

    let line_count = count_wrapped_lines(&text, inner_width);
    let max_scroll = line_count.saturating_sub(visible_lines);
    *scroll = (*scroll).min(max_scroll);

    let paragraph = Paragraph::new(text)
        .block(block)
        .style(style)
        .scroll((*scroll, 0))
        .wrap(Wrap { trim: false });
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
    let inner_width = inner.width.max(1) as usize;

    // Count wrapped lines by summing each line's wrapped height
    let line_count: u16 = text
        .lines
        .iter()
        .map(|line| {
            let len: usize = line.spans.iter().map(|s| s.content.len()).sum();
            if len == 0 {
                1u16
            } else {
                ((len + inner_width - 1) / inner_width) as u16
            }
        })
        .sum();

    let max_scroll = line_count.saturating_sub(visible_lines);
    *scroll = (*scroll).min(max_scroll);

    let paragraph = Paragraph::new(text)
        .block(block)
        .scroll((*scroll, 0))
        .wrap(Wrap { trim: false });
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

// ── Top-level entry point ─────────────────────────────────────────────────────

pub fn ui(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(5),
        ])
        .split(frame.area());

    let title_block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default());

    let title = Paragraph::new(Text::styled(
        "= ImHoTTeP =",
        Style::default().fg(Color::Green),
    ))
    .centered()
    .block(title_block);

    frame.render_widget(title, chunks[0]);

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

    let method_url_block = Block::default()
        .borders(Borders::ALL)
        .title("Method & URL (m=method, e/enter=edit url)")
        .border_style(focused_border_style(is_url_focused));

    let method_url = Paragraph::new(method_url_text)
        .block(method_url_block)
        .style(method_url_style);
    frame.render_widget(method_url, left_chunks[0]);

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
        if app.editing_header_key {
            lines.push(format!("Key: {} [EDITING]", app.header_key_buffer));
        } else {
            lines.push(format!("Key: {}", app.header_key_buffer));
            lines.push(format!("Value: {} [EDITING]", app.header_value_buffer));
        }

        let headers_block = Block::default()
            .borders(Borders::ALL)
            .title("Headers (tab=cycle name/val, enter=confirm, esc=cancel)")
            .border_style(Style::default().fg(Color::Cyan));

        let headers = Paragraph::new(lines.join("\n"))
            .block(headers_block)
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(headers, left_chunks[1]);

        if app.editing_header_key {
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

        let headers_title = if is_headers_focused {
            "Headers (a=add, e=edit, d=delete, ↑/↓=select)"
        } else {
            "Headers"
        };

        let headers_block = Block::default()
            .borders(Borders::ALL)
            .title(headers_title)
            .border_style(focused_border_style(is_headers_focused));

        frame.render_widget(List::new(header_items).block(headers_block), left_chunks[1]);
    }

    // ── Body ──────────────────────────────────────────────────────────────────
    let is_body_focused = app.focused_field == FocusableField::Body;
    let is_body_editing = app.editing_field == Some(EditingField::Body);

    let body_text = if is_body_editing {
        format!(
            "{} [EDITING - Ctrl+S to save, ESC to cancel]",
            app.input_buffer
        )
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

    let body_block = Block::default()
        .borders(Borders::ALL)
        .title("Body (e/enter=edit)")
        .border_style(focused_border_style(is_body_focused));

    render_scrollable_paragraph(
        frame,
        body_text,
        body_block,
        body_style,
        left_chunks[2],
        &mut app.body_scroll,
    );

    // ── Request Events ────────────────────────────────────────────────────────
    let is_events_focused = app.focused_field == FocusableField::RequestEvents;

    let events_text = if app.request_events.is_empty() {
        "(no events yet)".to_string()
    } else {
        app.request_events.join("\n")
    };

    let events_style = if app.pending_response.is_some() {
        Style::default().fg(Color::Cyan)
    } else if is_events_focused {
        Style::default().fg(Color::Gray).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let events_block = Block::default()
        .borders(Borders::ALL)
        .title("Request Events")
        .border_style(focused_border_style(is_events_focused));

    render_scrollable_paragraph(
        frame,
        events_text,
        events_block,
        events_style,
        right_chunks[1],
        &mut app.events_scroll,
    );

    // ── Response ──────────────────────────────────────────────────────────────
    let is_response_focused = app.focused_field == FocusableField::Response;
    let is_json_mode = app.response_view_mode == ResponseViewMode::Json;
    let is_filter_editing = app.editing_field == Some(EditingField::JsonFilter);

    // Determine the foreground color from response state, then apply the
    // focus background once rather than repeating it in every branch.
    let response_fg = if app.pending_response.is_some() {
        Some(Color::Cyan)
    } else if let Some(Ok(ref resp)) = app.last_response {
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
    let status_span = if app.pending_response.is_some() {
        Span::styled("...", Style::default().fg(Color::Cyan))
    } else if let Some(ref result) = app.last_response {
        match result {
            Ok(resp) => {
                let color = if resp.is_success() {
                    Color::Green
                } else {
                    Color::Red
                };
                Span::styled(
                    format!("{} {}", resp.status, resp.status_text),
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
        .border_style(focused_border_style(is_response_focused));

    if is_json_mode && app.last_response.is_some() && app.pending_response.is_none() {
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
        let filter_display = if is_filter_editing {
            format!("{} [EDITING]", app.input_buffer)
        } else {
            app.jq_filter.clone()
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
            .title("jq filter (f=edit, enter=apply, esc=cancel)")
            .border_style(if is_filter_editing {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let filter_paragraph = Paragraph::new(filter_display)
            .block(filter_block)
            .style(filter_style);
        frame.render_widget(filter_paragraph, response_chunks[1]);
    } else {
        // Plain text mode (or pending / no response)
        let response_text = if app.pending_response.is_some() {
            "Sending request...".to_string()
        } else if let Some(ref result) = app.last_response {
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
        );
    }

    // ── Autocomplete (rendered last so it floats above everything) ────────────
    if let Some((area, suggestions)) = deferred_autocomplete {
        frame.render_widget(ratatui::widgets::Clear, area);
        let items: Vec<ListItem> = suggestions
            .iter()
            .enumerate()
            .map(|(idx, suggestion)| {
                let style = if idx == app.header_autocomplete_selected {
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

    let key_notes_footer = (match app.current_screen {
        CurrentScreen::Main => Paragraph::new(vec![
            Line::from("↓↑/jk - select | n - new | d - delete").centered(),
            Line::from(
                if let Some(status) = &app.last_save_status {
                    status.as_str()
                } else {
                    "s - save | enter - edit | q - quit"
                }
            ).centered(),
        ])
        .style(Style::default().fg(Color::Yellow)),
        CurrentScreen::Request => {
            if app.editing_field.is_some() {
                let edit_hint = match app.editing_field.unwrap() {
                                    EditingField::Body => "type to edit | Ctrl+S - save | esc - cancel",
                                    EditingField::Headers => {
                                        if app.editing_header_key {
                                            "type header key | tab - next | ↓↑ - select | enter - confirm | esc - cancel"
                                        } else {
                                            "type header value | ⇧tab - prev | enter - confirm | esc - cancel"
                                        }
                                    }
                                    EditingField::Url => "type URL | enter - confirm | esc - cancel",
                                    EditingField::JsonFilter => "type jq filter | enter - apply | esc - cancel",
                                };
                Paragraph::new(Line::from(edit_hint).centered())
                    .style(Style::default().fg(Color::Cyan))
            } else {
                let field_hints = match app.focused_field {
                    FocusableField::Url => "u - edit URL | tab - next field",
                    FocusableField::Headers => "a - add | d - delete | ↓↑/jk - navigate",
                    FocusableField::Body => "↓↑/jk - scroll",
                    FocusableField::RequestEvents => "↓↑/jk - scroll",
                    FocusableField::Response => {
                        if app.response_view_mode == ResponseViewMode::Json {
                            "↓↑/jk - scroll | v - text mode | f - edit jq filter"
                        } else if app.is_response_json() {
                            "↓↑/jk - scroll | v - json mode"
                        } else {
                            "↓↑/jk - scroll"
                        }
                    }
                };

                Paragraph::new(vec![
                    Line::from("tab/⇧tab - navigate fields | e/enter - edit").centered(),
                    Line::from(field_hints).centered(),
                    Line::from("m - choose method | s - send request | q - back to list")
                        .centered(),
                ])
                .style(Style::default().fg(Color::Yellow))
            }
        }
        CurrentScreen::Exiting => {
            Paragraph::new(Line::from("y/enter - yes | n/q/esc - no").centered())
                .style(Style::default().fg(Color::Yellow))
        }
    })
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

    // ── count_wrapped_lines ───────────────────────────────────────────────────

    #[test]
    fn test_count_wrapped_lines_empty_string() {
        // An empty string has zero logical lines, so sum is 0
        assert_eq!(count_wrapped_lines("", 80), 0);
    }

    #[test]
    fn test_count_wrapped_lines_single_short_line() {
        assert_eq!(count_wrapped_lines("hello", 80), 1);
    }

    #[test]
    fn test_count_wrapped_lines_exactly_fits() {
        // 10 chars, width 10 → 1 row
        assert_eq!(count_wrapped_lines("1234567890", 10), 1);
    }

    #[test]
    fn test_count_wrapped_lines_wraps_to_two_rows() {
        // 11 chars in a width-10 box → 2 rows
        assert_eq!(count_wrapped_lines("12345678901", 10), 2);
    }

    #[test]
    fn test_count_wrapped_lines_empty_physical_line_counts_as_one() {
        // str::lines() treats a trailing newline as the end of the single preceding
        // empty line, so "\n" produces exactly 1 row.
        let text = "\n";
        assert_eq!(count_wrapped_lines(text, 80), 1);
    }

    #[test]
    fn test_count_wrapped_lines_two_newlines_give_two_lines() {
        // Two logical lines separated by a newline
        let text = "a\nb";
        assert_eq!(count_wrapped_lines(text, 80), 2);
    }

    #[test]
    fn test_count_wrapped_lines_multiple_short_lines() {
        let text = "line1\nline2\nline3";
        assert_eq!(count_wrapped_lines(text, 80), 3);
    }

    #[test]
    fn test_count_wrapped_lines_long_line_wraps_multiple_times() {
        // 30 chars, width 10 → 3 rows
        let text = "a".repeat(30);
        assert_eq!(count_wrapped_lines(&text, 10), 3);
    }

    #[test]
    fn test_count_wrapped_lines_mixed_long_and_short() {
        // 20-char line (2 rows at width 10) + 5-char line (1 row)
        let text = format!("{}\n{}", "x".repeat(20), "short");
        assert_eq!(count_wrapped_lines(&text, 10), 3);
    }

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
        let style = focused_border_style(true);
        assert_eq!(style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_focused_border_style_when_not_focused_is_default() {
        let style = focused_border_style(false);
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
}
