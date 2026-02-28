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
pub mod http_client;
mod ui;
use crate::{
    app::{App, CurrentScreen, EditingField, FocusableField, ResponseViewMode},
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
        let key_event = if app.pending_response.is_some() || app.event_receiver.is_some() {
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
            match app.current_screen {
                CurrentScreen::Main => match key.code {
                    KeyCode::Char('n') => {
                        app.create_new_request();
                    }
                    KeyCode::Char('d') => {
                        app.delete_current_request();
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.select_next_request();
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.select_previous_request();
                    }
                    KeyCode::Char('e') | KeyCode::Enter => {
                        if app.get_current_request().is_some() {
                            app.current_screen = CurrentScreen::Request;
                            app.editing_field = None;
                        }
                    }
                    KeyCode::Char('s') => {
                        app.save_requests();
                    }
                    KeyCode::Char('q') => {
                        app.current_screen = CurrentScreen::Exiting;
                    }
                    _ => {}
                },
                CurrentScreen::Exiting => match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => {
                        return Ok(false);
                    }
                    KeyCode::Char('n') | KeyCode::Char('q') | KeyCode::Esc => {
                        app.current_screen = CurrentScreen::Main;
                    }
                    _ => {}
                },
                CurrentScreen::Request => {
                    if let Some(editing) = app.editing_field {
                        // Handle input while editing a field
                        match key.code {
                            KeyCode::Esc => {
                                app.editing_field = None;
                                app.input_buffer.clear();
                                app.header_key_buffer.clear();
                                app.header_value_buffer.clear();
                                app.editing_header_key = true;
                                app.editing_existing_header = None;
                                app.header_autocomplete_visible = false;
                                app.header_autocomplete_selected = 0;
                            }
                            KeyCode::Tab | KeyCode::BackTab => {
                                // Toggle between header name and value fields
                                if editing == EditingField::Headers {
                                    app.editing_header_key = !app.editing_header_key;
                                    app.header_autocomplete_visible = app.editing_header_key;
                                    app.header_autocomplete_selected = 0;
                                }
                            }
                            KeyCode::Enter => {
                                match editing {
                                    EditingField::Url => {
                                        let url = app.input_buffer.clone();
                                        if let Some(request) = app.get_current_request_mut() {
                                            request.url = url;
                                        }
                                        app.editing_field = None;
                                        app.input_buffer.clear();
                                    }
                                    EditingField::Body => {
                                        // Allow newlines in body
                                        app.input_buffer.push('\n');
                                    }
                                    EditingField::JsonFilter => {
                                        // Confirm the filter and leave edit mode
                                        app.jq_filter = app.input_buffer.clone();
                                        app.editing_field = None;
                                        app.input_buffer.clear();
                                        app.response_scroll = 0;
                                    }
                                    EditingField::Headers => {
                                        // If autocomplete is visible, apply selection
                                        if app.header_autocomplete_visible {
                                            let suggestions = app.get_filtered_header_suggestions();
                                            app.apply_autocomplete_selection(&suggestions);
                                        } else if app.editing_header_key {
                                            // Move to value field
                                            app.editing_header_key = false;
                                        } else {
                                            // Save header
                                            let key = app.header_key_buffer.clone();
                                            let value = app.header_value_buffer.clone();
                                            let old_key = app.editing_existing_header.clone();

                                            if let Some(request) = app.get_current_request_mut() {
                                                if !key.is_empty() {
                                                    // If editing an existing header, remove the old one first
                                                    if let Some(ref old_key) = old_key {
                                                        request.remove_header(old_key);
                                                    }
                                                    request.add_header(key, value);
                                                }
                                            }
                                            app.editing_field = None;
                                            app.header_key_buffer.clear();
                                            app.header_value_buffer.clear();
                                            app.editing_header_key = true;
                                            app.editing_existing_header = None;
                                            app.header_autocomplete_visible = false;
                                            app.header_autocomplete_selected = 0;
                                        }
                                    }
                                }
                            }
                            KeyCode::Down => {
                                // Navigate autocomplete suggestions
                                if editing == EditingField::Headers
                                    && app.header_autocomplete_visible
                                {
                                    let suggestions = app.get_filtered_header_suggestions();
                                    app.select_next_autocomplete(suggestions.len());
                                }
                            }
                            KeyCode::Up => {
                                // Navigate autocomplete suggestions
                                if editing == EditingField::Headers
                                    && app.header_autocomplete_visible
                                {
                                    app.select_previous_autocomplete();
                                }
                            }
                            KeyCode::Char('s')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                // Ctrl+S to save body
                                if editing == EditingField::Body {
                                    let body = app.input_buffer.clone();
                                    if let Some(request) = app.get_current_request_mut() {
                                        request.set_body(body);
                                    }
                                    app.editing_field = None;
                                    app.input_buffer.clear();
                                }
                            }
                            KeyCode::Char(c) => match editing {
                                EditingField::Headers => {
                                    if app.editing_header_key {
                                        app.header_key_buffer.push(c);
                                        app.header_autocomplete_selected = 0;
                                    } else {
                                        app.header_value_buffer.push(c);
                                    }
                                }
                                _ => {
                                    app.input_buffer.push(c);
                                }
                            },
                            KeyCode::Backspace => match editing {
                                EditingField::Headers => {
                                    if app.editing_header_key {
                                        app.header_key_buffer.pop();
                                        app.header_autocomplete_selected = 0;
                                    } else {
                                        app.header_value_buffer.pop();
                                    }
                                }
                                _ => {
                                    app.input_buffer.pop();
                                }
                            },
                            _ => {}
                        }
                    } else {
                        // Navigation mode in Request screen
                        match key.code {
                            KeyCode::Tab => {
                                app.focus_next_field();
                            }
                            KeyCode::BackTab => {
                                app.focus_previous_field();
                            }
                            KeyCode::Char('e') | KeyCode::Enter => {
                                // If focused on headers, edit the selected one
                                if app.focused_field == FocusableField::Headers {
                                    app.edit_selected_header();
                                } else {
                                    app.edit_focused_field();
                                }
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                // If focused on headers, navigate header list
                                if app.focused_field == FocusableField::Headers {
                                    app.select_next_header();
                                } else {
                                    app.scroll_down();
                                }
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                // If focused on headers, navigate header list
                                if app.focused_field == FocusableField::Headers {
                                    app.select_previous_header();
                                } else {
                                    app.scroll_up();
                                }
                            }
                            KeyCode::Char('a') => {
                                // Add new header (only when focused on headers)
                                if app.focused_field == FocusableField::Headers {
                                    app.edit_focused_field();
                                }
                            }
                            KeyCode::Char('d') => {
                                // Delete selected header
                                if app.focused_field == FocusableField::Headers {
                                    app.delete_selected_header();
                                }
                            }
                            KeyCode::Esc | KeyCode::Char('q') => {
                                app.reset_request_screen_state();
                                app.current_screen = CurrentScreen::Main;
                            }
                            KeyCode::Char('u') => {
                                app.focused_field = FocusableField::Url;
                                app.edit_focused_field();
                            }
                            KeyCode::Char('m') => {
                                app.toggle_method();
                            }
                            KeyCode::Char('h') => {
                                app.focused_field = FocusableField::Headers;
                            }
                            KeyCode::Char('b') => {
                                app.focused_field = FocusableField::Body;
                                app.edit_focused_field();
                            }
                            KeyCode::Char('s') => {
                                app.send_current_request();
                            }
                            KeyCode::Char('v') => {
                                // Cycle response view mode (json mode only if body is valid json)
                                app.cycle_response_view_mode();
                                app.response_scroll = 0;
                            }
                            KeyCode::Char('f') => {
                                // Edit jq filter (only when response panel is focused in json mode)
                                if app.focused_field == FocusableField::Response
                                    && app.response_view_mode == ResponseViewMode::Json
                                {
                                    app.input_buffer = app.jq_filter.clone();
                                    app.editing_field = Some(EditingField::JsonFilter);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}
