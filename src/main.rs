use std::{error::Error, io};

use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
};

mod app;
mod keymap;
pub mod http_client;
mod ui;
use crate::{
    app::{App, CurrentScreen, EditingField, FocusableField, HeaderField, ResponseViewMode},
    keymap::{Action, Action::EditStreamPrefixRegex, Action::EditStreamSuffixRegex},
    ui::ui,
};

fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let res = run_app(&mut terminal, &mut app);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Ok(do_print) = res {
        if do_print {
            println!("The app wanted to print something")
        }
    } else if let Err(err) = res {
        eprintln!("{err:?}");
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<bool>
where
    std::io::Error: From<<B as Backend>::Error>,
{
    loop {
        // Check for pending HTTP responses and events
        app.check_pending_response();
        let received_events = app.check_for_events();

        terminal.draw(|f| ui(f, app))?;

        // If we have a pending response, use non-blocking event read
        // so we can check for new events frequently
        let has_async_work = app.pending_response.is_some()
            || app.event_receiver.is_some()
            || app.streamed_jq_output_rx.is_some();
        let key_event = if has_async_work {
            if event::poll(std::time::Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    Some(key)
                } else {
                    None
                }
            } else {
                // No key event, but continue loop to check for HTTP events
                if received_events {
                    // Force redraw if we received events
                    continue;
                }
                None
            }
        } else {
            // Blocking read when no pending request
            if let Event::Key(key) = event::read()? {
                Some(key)
            } else {
                None
            }
        };

        if let Some(key) = key_event {
            if key.kind == event::KeyEventKind::Release {
                continue;
            }

            let ctx = app.key_context();

            // Check if this is a typed character that should go to the input
            // buffer (not handled by the keymap). We only do this in editing mode.
            let is_editing = app.editing_field.is_some();
            if is_editing {
                if let KeyCode::Char(c) = key.code {
                    // Only treat as input if it's not a keymap-bound action.
                    // Ctrl+C and Ctrl+S are bound; plain chars are input.
                    if key.modifiers == event::KeyModifiers::NONE
                        || key.modifiers == event::KeyModifiers::SHIFT
                    {
                        // Route the character to the appropriate buffer.
                        if let Some(editing) = app.editing_field {
                            match editing {
                                EditingField::Headers => {
                                    if app.header_field == HeaderField::Key {
                                        app.header_key_buffer.insert(app.header_key_cursor, c);
                                        app.header_key_cursor += 1;
                                        if let Some(ref mut ac) = app.header_autocomplete {
                                            ac.selected = 0;
                                        }
                                    } else {
                                        app.header_value_buffer.insert(app.header_value_cursor, c);
                                        app.header_value_cursor += 1;
                                    }
                                    app.headers_scroll = crate::app::SCROLL_TO_END;
                                }
                                EditingField::Url => {
                                    app.input_buffer.insert(app.cursor_pos, c);
                                    app.cursor_pos += 1;
                                    app.url_h_scroll = crate::app::SCROLL_TO_END;
                                }
                                EditingField::JsonFilter
                                | EditingField::StreamPrefixRegex
                                | EditingField::StreamSuffixRegex => {
                                    app.input_buffer.insert(app.cursor_pos, c);
                                    app.cursor_pos += 1;
                                    app.filter_h_scroll = crate::app::SCROLL_TO_END;
                                }
                                EditingField::Body => {
                                    app.input_buffer.insert(app.cursor_pos, c);
                                    app.cursor_pos += 1;
                                }
                            }
                        }
                        continue;
                    }
                }
            }

            if let Some(action) = app.keymap.resolve(&ctx, &key) {
                if !execute_action(app, action)? {
                    return Ok(false);
                }
            }
        }
    }
}

/// Execute an action and return `Ok(true)` to continue the loop, or
/// `Ok(false)` to terminate the application.
fn execute_action(app: &mut App, action: Action) -> io::Result<bool> {
    match action {
        // ── Global ────────────────────────────────────────────────────────────
        Action::TriggerExit => {
            app.current_screen = CurrentScreen::Exiting;
        }

        // ── Main screen ───────────────────────────────────────────────────────
        Action::NewRequest => {
            app.create_new_request();
        }
        Action::DeleteRequest => {
            app.delete_current_request();
        }
        Action::SelectNextRequest => {
            app.select_next_request();
        }
        Action::SelectPreviousRequest => {
            app.select_previous_request();
        }
        Action::EditRequest => {
            if app.get_current_request().is_some() {
                app.current_screen = CurrentScreen::Request;
                app.editing_field = None;
            }
        }
        Action::SaveRequests => {
            app.save_requests();
        }

        // ── Exit confirmation ─────────────────────────────────────────────────
        Action::ConfirmExit => {
            return Ok(false);
        }
        Action::CancelExit => {
            app.current_screen = CurrentScreen::Main;
        }

        // ── Request screen — navigation ───────────────────────────────────────
        Action::FocusNextField => {
            app.focus_next_field();
        }
        Action::FocusPreviousField => {
            app.focus_previous_field();
        }
        Action::EditFocusedField => {
            app.edit_focused_field();
        }
        Action::EditSelectedHeader => {
            app.edit_selected_header();
        }
        Action::ScrollDown => {
            app.scroll_down(1);
        }
        Action::ScrollUp => {
            app.scroll_up(1);
        }
        Action::PageDown => {
            app.scroll_down(30);
        }
        Action::PageUp => {
            app.scroll_up(30);
        }
        Action::GoBack => {
            app.reset_request_screen_state();
            app.current_screen = CurrentScreen::Main;
        }
        Action::AddHeader => {
            if app.focused_field == FocusableField::Headers {
                app.edit_focused_field();
            }
        }
        Action::DeleteHeader => {
            if app.focused_field == FocusableField::Headers {
                app.delete_selected_header();
            }
        }
        Action::SelectNextHeader => {
            app.select_next_header();
        }
        Action::SelectPreviousHeader => {
            app.select_previous_header();
        }
        Action::ToggleMethod => {
            app.toggle_method();
        }
        Action::JumpToUrl => {
            app.focused_field = FocusableField::Url;
            app.edit_focused_field();
        }
        Action::FocusHeaders => {
            app.focused_field = FocusableField::Headers;
        }
        Action::JumpToBody => {
            app.focused_field = FocusableField::Body;
            app.edit_focused_field();
        }
        Action::SendRequest => {
            app.send_current_request();
        }
        Action::CycleViewMode => {
            app.cycle_response_view_mode();
            app.response_scroll = 0;
        }
        Action::EditJqFilter => {
            if app.focused_field == FocusableField::Response
                && (app.response_view_mode == ResponseViewMode::Json
                    || app.response_view_mode == ResponseViewMode::StreamedJson)
            {
                app.input_buffer = app.current_jq_filter().to_string();
                app.cursor_pos = app.input_buffer.len();
                app.editing_field = Some(EditingField::JsonFilter);
            }
        }
        EditStreamPrefixRegex => {
            if app.focused_field == FocusableField::Response
                && app.response_view_mode == ResponseViewMode::StreamedJson
            {
                app.input_buffer = app.current_stream_prefix_regex().to_string();
                app.cursor_pos = app.input_buffer.len();
                app.editing_field = Some(EditingField::StreamPrefixRegex);
            }
        }
        EditStreamSuffixRegex => {
            if app.focused_field == FocusableField::Response
                && app.response_view_mode == ResponseViewMode::StreamedJson
            {
                app.input_buffer = app.current_stream_suffix_regex().to_string();
                app.cursor_pos = app.input_buffer.len();
                app.editing_field = Some(EditingField::StreamSuffixRegex);
            }
        }

        // ── Editing mode ──────────────────────────────────────────────────────
        Action::CancelEdit => {
            app.editing_field = None;
            app.input_buffer.clear();
            app.cursor_pos = 0;
            app.header_key_buffer.clear();
            app.header_key_cursor = 0;
            app.header_value_buffer.clear();
            app.header_value_cursor = 0;
            app.header_field = HeaderField::Key;
            app.editing_existing_header = None;
            app.header_autocomplete = None;
        }
        Action::ConfirmEdit => {
            match app.editing_field {
                Some(EditingField::Url) => {
                    let url = app.input_buffer.clone();
                    if let Some(request) = app.get_current_request_mut() {
                        request.url = crate::http_client::UrlString::new(url);
                    }
                    app.editing_field = None;
                    app.input_buffer.clear();
                }
                Some(EditingField::JsonFilter) => {
                    let val = app.input_buffer.clone();
                    if let Some(request) = app.get_current_request_mut() {
                        request.jq_filter = crate::http_client::JqFilter::new(val);
                    }
                    app.editing_field = None;
                    app.input_buffer.clear();
                    app.response_scroll = 0;
                    // Re-process streamed lines if in StreamedJson mode
                    if app.response_view_mode == ResponseViewMode::StreamedJson {
                        app.reprocess_streamed_jq();
                    }
                }
                Some(EditingField::StreamPrefixRegex) => {
                    let val = app.input_buffer.clone();
                    if let Some(request) = app.get_current_request_mut() {
                        request.stream_prefix_regex = crate::http_client::RegexPattern::new(val);
                    }
                    app.editing_field = None;
                    app.input_buffer.clear();
                    app.response_scroll = 0;
                    app.reprocess_streamed_jq();
                }
                Some(EditingField::StreamSuffixRegex) => {
                    let val = app.input_buffer.clone();
                    if let Some(request) = app.get_current_request_mut() {
                        request.stream_suffix_regex = crate::http_client::RegexPattern::new(val);
                    }
                    app.editing_field = None;
                    app.input_buffer.clear();
                    app.response_scroll = 0;
                    app.reprocess_streamed_jq();
                }
                Some(EditingField::Headers) => {
                    if app.header_autocomplete.is_some() {
                        let suggestions = app.get_filtered_header_suggestions();
                        app.apply_autocomplete_selection(&suggestions);
                    } else if app.header_field == HeaderField::Key {
                        app.header_field = HeaderField::Value;
                        app.header_autocomplete = None;
                    } else {
                        let key = app.header_key_buffer.clone();
                        let value = app.header_value_buffer.clone();
                        let old_key = app.editing_existing_header.clone();
                        if let Some(request) = app.get_current_request_mut() {
                            if !key.is_empty() {
                                if let Some(ref old_key) = old_key {
                                    request.remove_header(old_key);
                                }
                                request.add_header(key, value);
                            }
                        }
                        app.editing_field = None;
                        app.header_key_buffer.clear();
                        app.header_value_buffer.clear();
                        app.header_field = HeaderField::Key;
                        app.editing_existing_header = None;
                        app.header_autocomplete = None;
                    }
                }
                _ => {}
            }
        }
        Action::ToggleHeaderKeyValue => {
            if app.editing_field == Some(EditingField::Headers) {
                app.header_field = match app.header_field {
                    HeaderField::Key => HeaderField::Value,
                    HeaderField::Value => HeaderField::Key,
                };
                if app.header_field == HeaderField::Key {
                    app.header_autocomplete = Some(crate::app::AutocompleteState { selected: 0 });
                } else {
                    app.header_autocomplete = None;
                }
            }
        }
        Action::InsertNewline => {
            app.input_buffer.insert(app.cursor_pos, '\n');
            app.cursor_pos += 1;
        }
        Action::SaveBody => {
            let body = app.input_buffer.clone();
            if let Some(request) = app.get_current_request_mut() {
                request.set_body(body);
            }
            app.editing_field = None;
            app.input_buffer.clear();
        }
        Action::DeleteChar => {
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                    if app.header_field == HeaderField::Key {
                        if app.header_key_cursor > 0 {
                            app.header_key_buffer.remove(app.header_key_cursor - 1);
                            app.header_key_cursor -= 1;
                            if let Some(ref mut ac) = app.header_autocomplete {
                                ac.selected = 0;
                            }
                        }
                    } else if app.header_value_cursor > 0 {
                            app.header_value_buffer.remove(app.header_value_cursor - 1);
                            app.header_value_cursor -= 1;
                        }
                        app.headers_scroll = crate::app::SCROLL_TO_END;
                    }
                    EditingField::Url
                    | EditingField::JsonFilter
                    | EditingField::StreamPrefixRegex
                    | EditingField::StreamSuffixRegex
                    | EditingField::Body => {
                        if app.cursor_pos > 0 {
                            app.input_buffer.remove(app.cursor_pos - 1);
                            app.cursor_pos -= 1;
                        }
                        match editing {
                            EditingField::Url => app.url_h_scroll = crate::app::SCROLL_TO_END,
                            EditingField::JsonFilter
                            | EditingField::StreamPrefixRegex
                            | EditingField::StreamSuffixRegex => {
                                app.filter_h_scroll = crate::app::SCROLL_TO_END;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Action::DeleteNextChar => {
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key {
                            if app.header_key_cursor < app.header_key_buffer.len() {
                                app.header_key_buffer.remove(app.header_key_cursor);
                            }
                        } else if app.header_value_cursor < app.header_value_buffer.len() {
                            app.header_value_buffer.remove(app.header_value_cursor);
                        }
                    }
                    _ => {
                        if app.cursor_pos < app.input_buffer.len() {
                            app.input_buffer.remove(app.cursor_pos);
                        }
                    }
                }
            }
        }
        Action::DeleteWordBackward => {
            let delete_before = |buf: &mut String, cursor: &mut usize| {
                if *cursor == 0 {
                    return;
                }
                let end = crate::app::word_boundary_before(buf, *cursor);
                buf.drain(end..*cursor);
                *cursor = end;
            };
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key {
                            delete_before(&mut app.header_key_buffer, &mut app.header_key_cursor);
                        } else {
                            delete_before(&mut app.header_value_buffer, &mut app.header_value_cursor);
                        }
                        app.headers_scroll = crate::app::SCROLL_TO_END;
                    }
                    _ => {
                        delete_before(&mut app.input_buffer, &mut app.cursor_pos);
                    }
                }
            }
        }
        Action::DeleteWordForward => {
            let delete_forward = |buf: &mut String, cursor: &mut usize| {
                if *cursor >= buf.len() {
                    return;
                }
                let end = crate::app::word_boundary_after(buf, *cursor);
                buf.drain(*cursor..end);
            };
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key {
                            delete_forward(
                                &mut app.header_key_buffer,
                                &mut app.header_key_cursor,
                            );
                        } else {
                            delete_forward(
                                &mut app.header_value_buffer,
                                &mut app.header_value_cursor,
                            );
                        }
                        app.headers_scroll = crate::app::SCROLL_TO_END;
                    }
                    _ => delete_forward(&mut app.input_buffer, &mut app.cursor_pos),
                }
            }
        }
        Action::CursorLeft => {
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key && app.header_key_cursor > 0 {
                            app.header_key_cursor -= 1;
                        } else if app.header_field == HeaderField::Value && app.header_value_cursor > 0 {
                            app.header_value_cursor -= 1;
                        }
                    }
                    _ => {
                        if app.cursor_pos > 0 {
                            app.cursor_pos -= 1;
                        }
                    }
                }
            }
        }
        Action::CursorRight => {
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key
                            && app.header_key_cursor < app.header_key_buffer.len()
                        {
                            app.header_key_cursor += 1;
                        } else if app.header_field == HeaderField::Value
                            && app.header_value_cursor < app.header_value_buffer.len()
                        {
                            app.header_value_cursor += 1;
                        }
                    }
                    _ => {
                        if app.cursor_pos < app.input_buffer.len() {
                            app.cursor_pos += 1;
                        }
                    }
                }
            }
        }
        Action::CursorWordLeft => {
            let word_left = |buf: &str, cursor: &mut usize| {
                if *cursor == 0 {
                    return;
                }
                *cursor = crate::app::word_boundary_before(buf, *cursor);
            };
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key {
                            word_left(&app.header_key_buffer, &mut app.header_key_cursor);
                        } else {
                            word_left(&app.header_value_buffer, &mut app.header_value_cursor);
                        }
                    }
                    _ => word_left(&app.input_buffer, &mut app.cursor_pos),
                }
            }
        }
        Action::CursorWordRight => {
            let word_right = |buf: &str, cursor: &mut usize| {
                if *cursor >= buf.len() {
                    *cursor = buf.len();
                    return;
                }
                *cursor = crate::app::word_boundary_after(buf, *cursor);
            };
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key {
                            word_right(&app.header_key_buffer, &mut app.header_key_cursor);
                        } else {
                            word_right(&app.header_value_buffer, &mut app.header_value_cursor);
                        }
                    }
                    _ => word_right(&app.input_buffer, &mut app.cursor_pos),
                }
            }
        }
        Action::CursorHome => {
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key {
                            app.header_key_cursor = 0;
                        } else {
                            app.header_value_cursor = 0;
                        }
                    }
                    _ => app.cursor_pos = 0,
                }
            }
        }
        Action::CursorEnd => {
            if let Some(editing) = app.editing_field {
                match editing {
                    EditingField::Headers => {
                        if app.header_field == HeaderField::Key {
                            app.header_key_cursor = app.header_key_buffer.len();
                        } else {
                            app.header_value_cursor = app.header_value_buffer.len();
                        }
                    }
                    _ => app.cursor_pos = app.input_buffer.len(),
                }
            }
        }
        Action::AutocompleteDown => {
            if app.editing_field == Some(EditingField::Headers)
                && app.header_autocomplete.is_some()
            {
                let suggestions = app.get_filtered_header_suggestions();
                app.select_next_autocomplete(suggestions.len());
            }
        }
        Action::AutocompleteUp => {
            if app.editing_field == Some(EditingField::Headers)
                && app.header_autocomplete.is_some()
            {
                app.select_previous_autocomplete();
            }
        }
    }

    Ok(true)
}
