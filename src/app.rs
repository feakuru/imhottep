use crate::http_client::{HttpError, HttpRequest, HttpResponse, HttpRuntime, RequestEvent};
use crate::keymap::{KeyContext, Keymap};
use hyper::Method;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

/// Sentinel value meaning "scroll to the last line on next render".
/// The renderer clamps scroll values to `max_scroll`, so any value larger
/// than the actual content height achieves the same effect.
pub const SCROLL_TO_END: u16 = u16::MAX;

// ── Word-boundary helpers ─────────────────────────────────────────────────────

/// Find the byte offset of the word boundary before `cursor` in `s`.
/// Words are delimited by alphanumeric category changes and whitespace.
pub fn word_boundary_before(s: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let before = &s[..cursor];
    let mut byte_end = cursor;
    let mut chars = before.chars().rev();
    // skip trailing whitespace
    while let Some(c) = chars.next() {
        if !c.is_whitespace() {
            byte_end -= c.len_utf8();
            break;
        }
        byte_end -= c.len_utf8();
    }
    if byte_end == 0 {
        return 0;
    }
    // determine category of first non-whitespace char
    let first = before[byte_end..].chars().next().unwrap();
    let is_alnum = first.is_alphanumeric();
    // consume same-category chars
    let mut pos = byte_end;
    for c in before[..byte_end].chars().rev() {
        if c.is_whitespace() || c.is_alphanumeric() != is_alnum {
            break;
        }
        pos -= c.len_utf8();
    }
    pos
}

/// Find the byte offset of the word boundary after `cursor` in `s`.
#[allow(dead_code)]
pub fn word_boundary_after(s: &str, cursor: usize) -> usize {
    if cursor >= s.len() {
        return cursor;
    }
    let after = &s[cursor..];
    let mut i = 0;
    // skip leading whitespace
    for c in after.chars() {
        if !c.is_whitespace() {
            break;
        }
        i += c.len_utf8();
    }
    if i >= after.len() {
        return s.len();
    }
    let first = after[i..].chars().next().unwrap();
    let is_alnum = first.is_alphanumeric();
    for c in after[i..].chars() {
        if c.is_whitespace() || c.is_alphanumeric() != is_alnum {
            break;
        }
        i += c.len_utf8();
    }
    cursor + i
}

/// Free-function version of strip_line that doesn't borrow self.
/// Takes optional prefix/suffix regex patterns to use for stripping.
fn strip_line(
    raw: &str,
    prefix_re: &Option<crate::http_client::RegexPattern>,
    suffix_re: &Option<crate::http_client::RegexPattern>,
) -> Result<String, String> {
    let stripped = match prefix_re.as_ref().and_then(|r| r.compiled()) {
        Some(re) => re.replace(raw, "").into_owned(),
        None => {
            let re = prefix_re
                .as_ref()
                .ok_or("No prefix regex available")?
                .compile()
                .map_err(|_| {
                    format!(
                        "Invalid prefix regex: {}",
                        prefix_re.as_ref().map(|r| r.pattern()).unwrap_or("")
                    )
                })?;
            re.replace(raw, "").into_owned()
        }
    };
    let stripped = match suffix_re.as_ref().and_then(|r| r.compiled()) {
        Some(re) => re.replace(&stripped, "").into_owned(),
        None => {
            let re = suffix_re
                .as_ref()
                .ok_or("No suffix regex available")?
                .compile()
                .map_err(|_| {
                    format!(
                        "Invalid suffix regex: {}",
                        suffix_re.as_ref().map(|r| r.pattern()).unwrap_or("")
                    )
                })?;
            re.replace(&stripped, "").into_owned()
        }
    };
    Ok(stripped)
}

// ── Event / state enums ───────────────────────────────────────────────────────

/// Events produced by the jq reader threads (stdout → Output, stderr → Error).
#[derive(Debug)]
pub(crate) enum JqEvent {
    Output(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentScreen {
    Main,
    Request,
    Exiting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditingField {
    Url,
    Headers,
    Body,
    JsonFilter,
    StreamPrefixRegex,
    StreamSuffixRegex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseViewMode {
    Text,
    Json,
    StreamedJson,
}

impl ResponseViewMode {
    pub fn label(&self) -> &'static str {
        match self {
            ResponseViewMode::Text => "text",
            ResponseViewMode::Json => "json",
            ResponseViewMode::StreamedJson => "streamed json",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusableField {
    Url,
    Headers,
    Body,
    RequestEvents,
    Response,
}

/// Which header sub-field the user is currently editing (key or value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderField {
    Key,
    Value,
}

/// State of the header autocomplete dropdown.
#[derive(Debug, Clone)]
pub struct AutocompleteState {
    pub selected: usize,
}

/// A running jq subprocess with its stdin handle.
pub struct JqProcess {
    pub stdin: std::process::ChildStdin,
    pub child: std::process::Child,
}

impl Drop for JqProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Per-request response and streaming state.
#[derive(Debug, Clone)]
pub struct PerRequestState {
    pub events: Vec<String>,
    pub streamed_body: String,
    pub last_response: Option<Result<HttpResponse, HttpError>>,
    pub jq_output: Vec<String>,
    pub line_buffer: String,
    pub stripped_lines: Vec<String>,
    pub last_fed: String,
}

impl PerRequestState {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            streamed_body: String::new(),
            last_response: None,
            jq_output: Vec::new(),
            line_buffer: String::new(),
            stripped_lines: Vec::new(),
            last_fed: String::new(),
        }
    }
}

pub struct App {
    pub current_screen: CurrentScreen,
    pub requests: Vec<HttpRequest>,
    pub current_request_index: Option<usize>,
    pub pending_response: Option<oneshot::Receiver<Result<HttpResponse, HttpError>>>,
    pub pending_request_index: Option<usize>,
    pub event_receiver: Option<mpsc::UnboundedReceiver<RequestEvent>>,
    pub per_request: HashMap<usize, PerRequestState>,
    pub http_runtime: HttpRuntime,
    pub editing_field: Option<EditingField>,
    pub focused_field: FocusableField,
    pub input_buffer: String,
    pub cursor_pos: usize,
    pub header_key_buffer: String,
    pub header_key_cursor: usize,
    pub header_value_buffer: String,
    pub header_value_cursor: usize,
    pub header_field: HeaderField,
    pub editing_existing_header: Option<String>,
    pub selected_header_index: usize,
    pub header_autocomplete: Option<AutocompleteState>,
    pub response_view_mode: ResponseViewMode,
    pub headers_scroll: u16,
    pub body_scroll: u16,
    pub events_scroll: u16,
    pub response_scroll: u16,
    pub url_h_scroll: u16,
    pub filter_h_scroll: u16,
    pub last_save_status: Option<String>,
    pub keymap: Keymap,
    pub streamed_jq_available: bool,
    pub streamed_jq_output_tx: Option<mpsc::UnboundedSender<JqEvent>>,
    pub streamed_jq_output_rx: Option<mpsc::UnboundedReceiver<JqEvent>>,
    pub streamed_jq_process: Option<JqProcess>,
}

impl App {
    pub fn new() -> App {
        let requests = load_requests_from_dir(None).unwrap_or_default();
        let current_request_index = if requests.is_empty() { None } else { Some(0) };
        App {
            current_screen: CurrentScreen::Main,
            requests,
            current_request_index,
            pending_response: None,
            pending_request_index: None,
            event_receiver: None,
            per_request: HashMap::new(),
            http_runtime: HttpRuntime::new().expect("Failed to create HTTP runtime"),
            editing_field: None,
            focused_field: FocusableField::Url,
            input_buffer: String::new(),
            cursor_pos: 0,
            header_key_buffer: String::new(),
            header_key_cursor: 0,
            header_value_buffer: String::new(),
            header_value_cursor: 0,
            header_field: HeaderField::Key,
            editing_existing_header: None,
            selected_header_index: 0,
            header_autocomplete: None,
            response_view_mode: ResponseViewMode::Text,
            headers_scroll: 0,
            body_scroll: 0,
            events_scroll: 0,
            response_scroll: 0,
            url_h_scroll: 0,
            filter_h_scroll: 0,
            last_save_status: None,
            keymap: Keymap::default(),
            streamed_jq_available: false,
            streamed_jq_output_tx: None,
            streamed_jq_output_rx: None,
            streamed_jq_process: None,
        }
    }

    pub fn get_current_request(&self) -> Option<&HttpRequest> {
        self.current_request_index
            .and_then(|idx| self.requests.get(idx))
    }

    pub fn get_current_request_mut(&mut self) -> Option<&mut HttpRequest> {
        self.current_request_index
            .and_then(|idx| self.requests.get_mut(idx))
    }

    // ── Per-request filter/regex accessors ───────────────────────────────────

    pub fn current_jq_filter(&self) -> &str {
        self.get_current_request()
            .map(|r| r.jq_filter.as_ref())
            .unwrap_or(".")
    }

    pub fn current_stream_prefix_regex(&self) -> &str {
        self.get_current_request()
            .map(|r| r.stream_prefix_regex.pattern())
            .unwrap_or(r"^\w+:\s*")
    }

    pub fn current_stream_suffix_regex(&self) -> &str {
        self.get_current_request()
            .map(|r| r.stream_suffix_regex.pattern())
            .unwrap_or(r"\s*$")
    }

    pub fn create_new_request(&mut self) {
        let new_request = HttpRequest::new(Method::GET, "https://");
        self.requests.push(new_request);
        self.current_request_index = Some(self.requests.len() - 1);
    }

    pub fn delete_current_request(&mut self) {
        if let Some(idx) = self.current_request_index {
            if idx < self.requests.len() {
                self.requests.remove(idx);
                self.per_request.remove(&idx);
                // Shift keys for entries above the deleted index.
                self.per_request = self
                    .per_request
                    .drain()
                    .filter_map(|(k, v)| {
                        if k == idx {
                            None
                        } else if k > idx {
                            Some((k - 1, v))
                        } else {
                            Some((k, v))
                        }
                    })
                    .collect();
                if let Some(pending) = self.pending_request_index {
                    if pending == idx {
                        self.pending_response = None;
                        self.pending_request_index = None;
                        self.event_receiver = None;
                    } else if pending > idx {
                        self.pending_request_index = Some(pending - 1);
                    }
                }
                if self.requests.is_empty() {
                    self.current_request_index = None;
                } else if idx >= self.requests.len() {
                    self.current_request_index = Some(self.requests.len() - 1);
                }
            }
        }
    }

    pub fn select_next_request(&mut self) {
        if self.requests.is_empty() {
            return;
        }
        self.current_request_index = Some(match self.current_request_index {
            Some(idx) if idx < self.requests.len() - 1 => idx + 1,
            Some(idx) => idx,
            None => 0,
        });
    }

    pub fn select_previous_request(&mut self) {
        if self.requests.is_empty() {
            return;
        }
        self.current_request_index = Some(match self.current_request_index {
            Some(idx) if idx > 0 => idx - 1,
            Some(idx) => idx,
            None => 0,
        });
    }

    pub fn send_current_request(&mut self) {
        if let Some(idx) = self.current_request_index {
            if let Some(request) = self.requests.get(idx).cloned() {
                self.kill_streamed_jq();

                let (result_rx, event_rx) = self.http_runtime.execute_request(request);
                self.pending_response = Some(result_rx);
                self.pending_request_index = Some(idx);
                self.event_receiver = Some(event_rx);
                self.per_request.insert(idx, PerRequestState::new());
                self.streamed_jq_available = false;
            }
        }
    }

    pub fn check_pending_response(&mut self) {
        if let Some(receiver) = &mut self.pending_response {
            match receiver.try_recv() {
                Ok(result) => {
                    if let Some(idx) = self.pending_request_index {
                        let state = self
                            .per_request
                            .entry(idx)
                            .or_insert_with(PerRequestState::new);
                        match result {
                            Ok(resp) => {
                                state.streamed_body = resp.body.clone();
                                state.last_response = Some(Ok(resp));
                            }
                            Err(e) => {
                                state.last_response = Some(Err(e));
                            }
                        }
                    }
                    self.pending_response = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    if let Some(idx) = self.pending_request_index {
                        self.per_request
                            .entry(idx)
                            .or_insert_with(PerRequestState::new)
                            .last_response = Some(Err(HttpError::RequestFailed {
                            msg: "Request channel closed".to_string(),
                            source: None,
                        }));
                    }
                    self.pending_response = None;
                    self.pending_request_index = None;
                    self.streamed_jq_process = None;
                }
            }
        }
    }

    pub fn check_for_events(&mut self) -> bool {
        let mut received_any = false;

        let mut jq_channel_closed = false;
        if let Some(rx) = &mut self.streamed_jq_output_rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => {
                        if let Some(idx) = self.current_request_index {
                            let state = self
                                .per_request
                                .entry(idx)
                                .or_insert_with(PerRequestState::new);
                            match event {
                                JqEvent::Output(line) => {
                                    state.jq_output.push(line);
                                }
                                JqEvent::Error(err) => {
                                    let original = state.last_fed.clone();
                                    state
                                        .jq_output
                                        .push(format!("{original} \x1b[31m// jq: {err}\x1b[0m"));
                                }
                            }
                        }
                        received_any = true;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        jq_channel_closed = true;
                        break;
                    }
                }
            }
        }
        if jq_channel_closed {
            self.streamed_jq_output_rx = None;
            self.streamed_jq_output_tx = None;
            if let Some(mut jq) = self.streamed_jq_process.take() {
                let _ = jq.child.wait();
            }
        }

        let mut collected_events: Vec<RequestEvent> = Vec::new();
        let mut event_channel_closed = false;
        if let Some(receiver) = &mut self.event_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        collected_events.push(event);
                        received_any = true;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        event_channel_closed = true;
                        break;
                    }
                }
            }
        }

        let idx_opt = self.pending_request_index;
        for event in collected_events {
            if let Some(idx) = idx_opt {
                let state = self
                    .per_request
                    .entry(idx)
                    .or_insert_with(PerRequestState::new);
                match event {
                    crate::http_client::RequestEvent::BodyChunk(s) => {
                        state.streamed_body.push_str(&s);
                        state
                            .events
                            .push(format!("Response chunk received: {} bytes", s.len()));
                        self.process_chunk(idx, s);
                    }
                    other => {
                        state.events.push(other.to_string());
                    }
                }
            }
        }

        if event_channel_closed && self.pending_response.is_none() {
            if let Some(idx) = self.pending_request_index.take() {
                self.flush_line_buffer(idx);
            }
            self.streamed_jq_process = None;
            self.event_receiver = None;
        }

        received_any
    }

    pub fn toggle_method(&mut self) {
        if let Some(request) = self.get_current_request_mut() {
            request.method = match request.method {
                Method::GET => Method::POST,
                Method::POST => Method::PUT,
                Method::PUT => Method::PATCH,
                Method::PATCH => Method::DELETE,
                Method::DELETE => Method::HEAD,
                Method::HEAD => Method::OPTIONS,
                Method::OPTIONS => Method::GET,
                _ => Method::GET,
            };
        }
    }

    pub fn current_request_is_pending(&self) -> bool {
        self.pending_request_index == self.current_request_index && self.pending_response.is_some()
    }

    pub fn current_last_response(&self) -> Option<&Result<HttpResponse, HttpError>> {
        self.current_request_index
            .and_then(|idx| self.per_request.get(&idx))
            .and_then(|s| s.last_response.as_ref())
    }

    pub fn current_streamed_body(&self) -> &str {
        self.current_request_index
            .and_then(|idx| self.per_request.get(&idx))
            .map(|s| s.streamed_body.as_str())
            .unwrap_or("")
    }

    pub fn current_request_events(&self) -> &[String] {
        self.current_request_index
            .and_then(|idx| self.per_request.get(&idx))
            .map(|s| s.events.as_slice())
            .unwrap_or(&[])
    }

    pub fn is_response_json(&self) -> bool {
        if let Some(Ok(resp)) = self.current_last_response() {
            serde_json::from_str::<serde_json::Value>(&resp.body).is_ok()
        } else {
            false
        }
    }

    // ── Streamed-jq helpers ───────────────────────────────────────────────────

    /// Ensure the persistent jq subprocess is running (starting it if needed).
    /// Returns `true` if jq is ready to receive input.
    fn ensure_jq_running(&mut self) -> bool {
        if self.streamed_jq_process.is_some() {
            return true;
        }
        let filter = {
            let f = self.current_jq_filter();
            if f.trim().is_empty() {
                ".".to_string()
            } else {
                f.to_string()
            }
        };
        match std::process::Command::new("jq")
            .args(["--color-output", "--unbuffered", &filter])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let stdout = child.stdout.take().unwrap();
                let stderr = child.stderr.take().unwrap();
                let stdin = child.stdin.take().unwrap();

                // Spawn a reader thread that collects stdout + stderr and sends
                // them through an mpsc channel.
                let (tx, rx) = mpsc::unbounded_channel::<JqEvent>();
                let tx_stdout = tx.clone();
                let tx_err = tx.clone();
                let stdout_reader = BufReader::new(stdout);
                std::thread::spawn(move || {
                    for line in stdout_reader.lines() {
                        match line {
                            Ok(l) => {
                                if tx_stdout.send(JqEvent::Output(l)).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
                let stderr_reader = BufReader::new(stderr);
                std::thread::spawn(move || {
                    for line in stderr_reader.lines() {
                        match line {
                            Ok(l) => {
                                if tx_err.send(JqEvent::Error(l.trim().to_string())).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });

                self.streamed_jq_output_tx = Some(tx);
                self.streamed_jq_output_rx = Some(rx);
                self.streamed_jq_process = Some(JqProcess { stdin, child });
                true
            }
            Err(e) => {
                let msg = format!("\x1b[31m// Failed to start jq: {e}\x1b[0m");
                if let Some(idx) = self.current_request_index {
                    self.per_request
                        .entry(idx)
                        .or_insert_with(PerRequestState::new)
                        .jq_output
                        .push(msg);
                }
                false
            }
        }
    }

    /// Kill and clean up the jq subprocess if running.
    pub fn kill_streamed_jq(&mut self) {
        self.streamed_jq_process = None;
        self.streamed_jq_output_tx = None;
        self.streamed_jq_output_rx = None;
    }

    /// Send a single stripped line to the jq subprocess stdin.
    /// If the write fails (jq died after a parse error), we drain any remaining
    /// output from the old process, kill it, spawn a fresh one, and retry.
    fn feed_line_to_jq(&mut self, idx: usize, stripped: &str) {
        if !self.ensure_jq_running() {
            return;
        }
        let write_ok = if let Some(jq) = &mut self.streamed_jq_process {
            writeln!(jq.stdin, "{stripped}").is_ok()
        } else {
            false
        };
        if !write_ok {
            if let Some(rx) = &mut self.streamed_jq_output_rx {
                while let Ok(event) = rx.try_recv() {
                    let state = self
                        .per_request
                        .entry(idx)
                        .or_insert_with(PerRequestState::new);
                    match event {
                        JqEvent::Output(line) => state.jq_output.push(line),
                        JqEvent::Error(err) => {
                            let original = state.last_fed.clone();
                            state
                                .jq_output
                                .push(format!("{original} \x1b[31m// jq: {err}\x1b[0m"));
                        }
                    }
                }
            }
            self.kill_streamed_jq();
            if self.ensure_jq_running() {
                if let Some(jq) = &mut self.streamed_jq_process {
                    let _ = writeln!(jq.stdin, "{stripped}");
                }
            }
        }
        self.per_request
            .entry(idx)
            .or_insert_with(PerRequestState::new)
            .last_fed = stripped.to_string();
    }

    /// Run `jq` synchronously on a batch of pre-stripped JSON lines, returning
    /// display lines (colourised output or inline error annotations).
    ///
    /// Because jq exits with code 5 on the first parse error we restart it for
    /// each offending line and continue, so every line gets processed.
    fn run_jq_batch_sync(filter: &str, stripped_lines: &[String]) -> Vec<String> {
        let mut output: Vec<String> = Vec::new();
        let effective_filter = if filter.trim().is_empty() {
            "."
        } else {
            filter
        };

        // Spawn a fresh jq per line and wait for it to finish.  This is
        // slightly slower than keeping a persistent process, but it is
        // trivially correct: no output can be lost to timing races, and
        // a parse error on one line never affects subsequent lines.
        for line in stripped_lines {
            match std::process::Command::new("jq")
                .args(["--color-output", effective_filter])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Err(e) => {
                    output.push(format!("\x1b[31m// Failed to start jq: {e}\x1b[0m"));
                    break; // jq not available — no point continuing
                }
                Ok(mut child) => {
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = writeln!(stdin, "{line}");
                        // stdin drops here → EOF → jq processes and exits
                    }
                    match child.wait_with_output() {
                        Err(e) => {
                            output.push(format!("\x1b[31m// jq wait error: {e}\x1b[0m"));
                        }
                        Ok(result) => {
                            if result.status.success() {
                                let s = String::from_utf8_lossy(&result.stdout);
                                for out_line in s.lines() {
                                    output.push(out_line.to_string());
                                }
                            } else {
                                // Parse/filter error — show original line + error message.
                                let err = String::from_utf8_lossy(&result.stderr);
                                let err_trimmed = err.trim();
                                output.push(format!("{line} \x1b[31m// jq: {err_trimmed}\x1b[0m"));
                            }
                        }
                    }
                }
            }
        }

        output
    }

    fn process_chunk(&mut self, idx: usize, chunk: String) {
        let state = self
            .per_request
            .entry(idx)
            .or_insert_with(PerRequestState::new);
        state.line_buffer.push_str(&chunk);

        let combined = std::mem::take(&mut state.line_buffer);
        let mut parts: Vec<&str> = combined.split('\n').collect();
        let tail = parts.pop().unwrap_or("").to_string();
        state.line_buffer = tail;

        for raw_line in parts {
            self.handle_complete_line(idx, raw_line);
        }
    }

    fn handle_complete_line(&mut self, idx: usize, raw: &str) {
        let trimmed = raw.trim_end_matches('\r');
        // Extract regex before mutable borrow of per_request
        let prefix_re = self
            .get_current_request()
            .map(|r| r.stream_prefix_regex.clone());
        let suffix_re = self
            .get_current_request()
            .map(|r| r.stream_suffix_regex.clone());

        let strip_result = strip_line(trimmed, &prefix_re, &suffix_re);
        match strip_result {
            Err(e) => {
                let msg = format!("\x1b[31m// {e}\x1b[0m");
                self.per_request
                    .entry(idx)
                    .or_insert_with(PerRequestState::new)
                    .jq_output
                    .push(msg);
            }
            Ok(stripped) => {
                if stripped.is_empty() {
                    return;
                }
                if serde_json::from_str::<serde_json::Value>(&stripped).is_ok() {
                    self.streamed_jq_available = true;
                }
                self.per_request
                    .entry(idx)
                    .or_insert_with(PerRequestState::new)
                    .stripped_lines
                    .push(stripped.clone());
                self.feed_line_to_jq(idx, &stripped);
            }
        }
    }

    fn flush_line_buffer(&mut self, idx: usize) {
        let tail = self
            .per_request
            .get(&idx)
            .map(|s| s.line_buffer.clone())
            .unwrap_or_default();
        if !tail.is_empty() {
            self.handle_complete_line(idx, &tail);
            self.per_request
                .entry(idx)
                .or_insert_with(PerRequestState::new)
                .line_buffer
                .clear();
        }
    }

    pub fn reprocess_streamed_jq(&mut self) {
        let Some(idx) = self.current_request_index else {
            return;
        };
        // Extract regex patterns before the mutable borrow below.
        let prefix_re = self
            .get_current_request()
            .map(|r| r.stream_prefix_regex.clone());
        let suffix_re = self
            .get_current_request()
            .map(|r| r.stream_suffix_regex.clone());
        let filter = self.current_jq_filter().to_string();

        self.kill_streamed_jq();

        let state = self
            .per_request
            .entry(idx)
            .or_insert_with(PerRequestState::new);
        state.jq_output.clear();
        state.last_fed.clear();

        let body = state.streamed_body.clone();
        state.stripped_lines.clear();
        self.streamed_jq_available = false;

        let raw_lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
        let mut stripped_lines: Vec<String> = Vec::new();
        for raw in &raw_lines {
            let trimmed = raw.trim_end_matches('\r');
            match strip_line(trimmed, &prefix_re, &suffix_re) {
                Err(e) => {
                    state.jq_output.push(format!("\x1b[31m// {e}\x1b[0m"));
                }
                Ok(stripped) => {
                    if stripped.is_empty() {
                        continue;
                    }
                    if serde_json::from_str::<serde_json::Value>(&stripped).is_ok() {
                        self.streamed_jq_available = true;
                    }
                    state.stripped_lines.push(stripped.clone());
                    stripped_lines.push(stripped);
                }
            }
        }

        let batch_output = Self::run_jq_batch_sync(&filter, &stripped_lines);
        state.jq_output.extend(batch_output);
    }

    pub fn current_streamed_jq_output(&self) -> String {
        self.current_request_index
            .and_then(|idx| self.per_request.get(&idx))
            .map(|s| s.jq_output.join("\n"))
            .unwrap_or_default()
    }

    /// Cycles to the next available view mode.  Json mode is only entered when
    /// the response body is valid JSON; StreamedJson mode only when at least one
    /// stripped streamed line was valid JSON.
    pub fn cycle_response_view_mode(&mut self) {
        self.response_view_mode = match self.response_view_mode {
            ResponseViewMode::Text => {
                if self.is_response_json() {
                    ResponseViewMode::Json
                } else if self.streamed_jq_available {
                    ResponseViewMode::StreamedJson
                } else {
                    ResponseViewMode::Text
                }
            }
            ResponseViewMode::Json => {
                if self.streamed_jq_available {
                    ResponseViewMode::StreamedJson
                } else {
                    ResponseViewMode::Text
                }
            }
            ResponseViewMode::StreamedJson => ResponseViewMode::Text,
        };
    }

    pub fn run_jq(&self) -> String {
        let body = match self.current_last_response() {
            Some(Ok(resp)) => &resp.body,
            _ => return String::new(),
        };
        let filter = if self.current_jq_filter().trim().is_empty() {
            "."
        } else {
            self.current_jq_filter()
        };
        match std::process::Command::new("jq")
            .args(["--color-output", filter])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                use std::io::Write;
                if let Some(stdin) = child.stdin.take() {
                    let mut stdin = stdin;
                    let _ = stdin.write_all(body.as_bytes());
                }
                match child.wait_with_output() {
                    Ok(output) if output.status.success() => {
                        String::from_utf8_lossy(&output.stdout).into_owned()
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        format!("jq error: {}", stderr.trim())
                    }
                    Err(e) => format!("jq error: {e}"),
                }
            }
            Err(e) => format!("Failed to run jq: {e}"),
        }
    }

    pub fn focus_next_field(&mut self) {
        self.focused_field = match self.focused_field {
            FocusableField::Url => FocusableField::Headers,
            FocusableField::Headers => FocusableField::Body,
            FocusableField::Body => FocusableField::Response,
            FocusableField::Response => FocusableField::RequestEvents,
            FocusableField::RequestEvents => FocusableField::Url,
        };
    }

    pub fn focus_previous_field(&mut self) {
        self.focused_field = match self.focused_field {
            FocusableField::Url => FocusableField::RequestEvents,
            FocusableField::Headers => FocusableField::Url,
            FocusableField::Body => FocusableField::Headers,
            FocusableField::Response => FocusableField::Body,
            FocusableField::RequestEvents => FocusableField::Response,
        };
    }

    pub fn edit_focused_field(&mut self) {
        self.editing_field = Some(match self.focused_field {
            FocusableField::Url => {
                if let Some(request) = self.get_current_request() {
                    self.input_buffer = request.url.to_string();
                }
                self.cursor_pos = self.input_buffer.len();
                EditingField::Url
            }
            FocusableField::Headers => {
                self.header_key_buffer.clear();
                self.header_key_cursor = 0;
                self.header_value_buffer.clear();
                self.header_value_cursor = 0;
                self.header_field = HeaderField::Key;
                self.editing_existing_header = None;
                self.header_autocomplete = Some(AutocompleteState { selected: 0 });
                EditingField::Headers
            }
            FocusableField::Body => {
                if let Some(request) = self.get_current_request() {
                    self.input_buffer = request.body.clone().unwrap_or_default();
                }
                self.cursor_pos = self.input_buffer.len();
                EditingField::Body
            }
            FocusableField::RequestEvents | FocusableField::Response => {
                return;
            }
        });
    }

    pub fn select_next_header(&mut self) {
        if let Some(request) = self.get_current_request() {
            let header_count = request.headers.len();
            if header_count > 0 {
                self.selected_header_index = (self.selected_header_index + 1) % header_count;
            }
        }
    }

    pub fn select_previous_header(&mut self) {
        if let Some(request) = self.get_current_request() {
            let header_count = request.headers.len();
            if header_count > 0 {
                self.selected_header_index = self
                    .selected_header_index
                    .checked_sub(1)
                    .unwrap_or(header_count - 1);
            }
        }
    }

    pub fn edit_selected_header(&mut self) {
        let header_data = self.get_current_request().and_then(|request| {
            request
                .headers
                .iter()
                .nth(self.selected_header_index)
                .map(|(k, v)| (k.to_string(), v.to_string()))
        });

        if let Some((key, value)) = header_data {
            self.editing_existing_header = Some(key.clone());
            self.header_key_buffer = key;
            self.header_key_cursor = self.header_key_buffer.len();
            self.header_value_buffer = value;
            self.header_value_cursor = self.header_value_buffer.len();
            self.editing_field = Some(EditingField::Headers);
            self.header_field = HeaderField::Key;
            self.header_autocomplete = Some(AutocompleteState { selected: 0 });
        }
    }

    pub fn delete_selected_header(&mut self) {
        let selected_idx = self.selected_header_index;

        if let Some(request) = self.get_current_request_mut() {
            let keys: Vec<_> = request.headers.keys().map(|k| k.to_string()).collect();
            if let Some(key) = keys.get(selected_idx) {
                request.remove_header(key);
                let new_len = request.headers.len();
                if selected_idx >= new_len && selected_idx > 0 {
                    self.selected_header_index = selected_idx - 1;
                }
            }
        }
    }

    fn focused_scroll(&mut self) -> Option<&mut u16> {
        match self.focused_field {
            FocusableField::Headers => Some(&mut self.headers_scroll),
            FocusableField::Body => Some(&mut self.body_scroll),
            FocusableField::RequestEvents => Some(&mut self.events_scroll),
            FocusableField::Response => Some(&mut self.response_scroll),
            FocusableField::Url => None,
        }
    }

    pub fn scroll_up(&mut self, by: u16) {
        if let Some(scroll) = self.focused_scroll() {
            *scroll = scroll.saturating_sub(by);
        }
    }

    pub fn scroll_down(&mut self, by: u16) {
        if let Some(scroll) = self.focused_scroll() {
            *scroll = scroll.saturating_add(by);
        }
    }

    pub fn key_context(&self) -> KeyContext {
        KeyContext {
            screen: self.current_screen,
            editing: self.editing_field,
            focus: self.focused_field,
        }
    }

    pub fn reset_request_screen_state(&mut self) {
        self.editing_field = None;
        self.input_buffer.clear();
        self.cursor_pos = 0;
        self.header_key_buffer.clear();
        self.header_key_cursor = 0;
        self.header_value_buffer.clear();
        self.header_value_cursor = 0;
        self.header_field = HeaderField::Key;
        self.editing_existing_header = None;
        self.focused_field = FocusableField::Url;
        self.headers_scroll = 0;
        self.body_scroll = 0;
        self.events_scroll = 0;
        self.response_scroll = 0;
        self.url_h_scroll = 0;
        self.filter_h_scroll = 0;
        self.header_autocomplete = None;
    }

    pub fn get_filtered_header_suggestions(&self) -> Vec<&'static str> {
        if self.header_field != HeaderField::Key {
            return Vec::new();
        }

        let query = self.header_key_buffer.to_lowercase();
        let standard_headers = get_standard_headers();
        let mut suggestions: Vec<&'static str> = if query.is_empty() {
            standard_headers.iter().copied().collect()
        } else {
            standard_headers
                .iter()
                .copied()
                .filter(|header| fuzzy_match(&query, &header.to_lowercase()))
                .collect()
        };

        suggestions.sort_by(|a, b| {
            let a_lower = a.to_lowercase();
            let b_lower = b.to_lowercase();
            let a_starts = a_lower.starts_with(&query);
            let b_starts = b_lower.starts_with(&query);

            match (a_starts, b_starts) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.len().cmp(&b.len()),
            }
        });

        suggestions
    }

    pub fn select_next_autocomplete(&mut self, max: usize) {
        if let Some(ref mut ac) = self.header_autocomplete {
            if max > 0 && ac.selected < max - 1 {
                ac.selected += 1;
            }
        }
    }

    pub fn select_previous_autocomplete(&mut self) {
        if let Some(ref mut ac) = self.header_autocomplete {
            if ac.selected > 0 {
                ac.selected -= 1;
            }
        }
    }

    pub fn apply_autocomplete_selection(&mut self, suggestions: &[&str]) {
        let selected = self.header_autocomplete.as_ref().map(|ac| ac.selected);
        if let Some(sel) = selected {
            if let Some(selected_name) = suggestions.get(sel) {
                self.header_key_buffer = selected_name.to_string();
                self.header_field = HeaderField::Value;
                self.header_autocomplete = None;
            }
        }
    }

    pub fn save_requests(&mut self) {
        match save_requests_to_dir(&self.requests, None) {
            Ok(path) => {
                self.last_save_status = Some(format!("Saved to {}", path.display()));
            }
            Err(e) => {
                self.last_save_status = Some(format!("Save failed: {e}"));
            }
        }
    }
}

// ── Persistence ───────────────────────────────────────────────────────────────

fn library_path_with_base(base_override: Option<&std::path::Path>) -> PathBuf {
    let base = match base_override {
        Some(provided_base) => provided_base.to_path_buf(),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| {
                    let mut p = PathBuf::from(h);
                    p.push(".config");
                    p
                })
            })
            .unwrap_or_else(|| PathBuf::from(".")),
    };

    base.join("imhottep").join("request-library.json")
}

pub fn load_requests_from_dir(
    base: Option<&std::path::Path>,
) -> Result<Vec<HttpRequest>, Box<dyn std::error::Error>> {
    let path = library_path_with_base(base);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)?;
    let requests = serde_json::from_str(&data)?;
    Ok(requests)
}

pub fn save_requests_to_dir(
    requests: &[HttpRequest],
    base: Option<&std::path::Path>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = library_path_with_base(base);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(requests)?;
    std::fs::write(&path, data)?;
    Ok(path)
}

// Standard HTTP header names per IANA registry and common RFCs
fn get_standard_headers() -> &'static [&'static str] {
    &[
        "accept",
        "accept-charset",
        "accept-encoding",
        "accept-language",
        "accept-ranges",
        "access-control-allow-credentials",
        "access-control-allow-headers",
        "access-control-allow-methods",
        "access-control-allow-origin",
        "access-control-expose-headers",
        "access-control-max-age",
        "access-control-request-headers",
        "access-control-request-method",
        "age",
        "allow",
        "alt-svc",
        "authorization",
        "cache-control",
        "cache-status",
        "cdn-cache-control",
        "connection",
        "content-disposition",
        "content-encoding",
        "content-language",
        "content-length",
        "content-location",
        "content-range",
        "content-security-policy",
        "content-security-policy-report-only",
        "content-type",
        "cookie",
        "date",
        "dnt",
        "etag",
        "expect",
        "expires",
        "forwarded",
        "from",
        "host",
        "if-match",
        "if-modified-since",
        "if-none-match",
        "if-range",
        "if-unmodified-since",
        "last-modified",
        "link",
        "location",
        "max-forwards",
        "origin",
        "pragma",
        "proxy-authenticate",
        "proxy-authorization",
        "public-key-pins",
        "public-key-pins-report-only",
        "range",
        "referer",
        "referrer-policy",
        "refresh",
        "retry-after",
        "sec-websocket-accept",
        "sec-websocket-extensions",
        "sec-websocket-key",
        "sec-websocket-protocol",
        "sec-websocket-version",
        "server",
        "set-cookie",
        "strict-transport-security",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "upgrade-insecure-requests",
        "user-agent",
        "vary",
        "via",
        "warning",
        "www-authenticate",
        "x-content-type-options",
        "x-dns-prefetch-control",
        "x-frame-options",
        "x-xss-protection",
    ]
}

// Fuzzy matching helper - checks if query characters appear in target in order
fn fuzzy_match(query: &str, target: &str) -> bool {
    let mut target_chars = target.chars();
    query.chars().all(|qc| target_chars.any(|tc| tc == qc))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::{HttpMethod, HttpRequest, HttpResponse, RegexPattern};
    use std::collections::HashMap;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn app_with_requests(requests: Vec<HttpRequest>) -> App {
        let current_request_index = if requests.is_empty() { None } else { Some(0) };
        App {
            current_screen: CurrentScreen::Main,
            requests,
            current_request_index,
            pending_response: None,
            pending_request_index: None,
            event_receiver: None,
            per_request: HashMap::new(),
            http_runtime: HttpRuntime::new().expect("runtime"),
            editing_field: None,
            focused_field: FocusableField::Url,
            input_buffer: String::new(),
            cursor_pos: 0,
            header_key_buffer: String::new(),
            header_key_cursor: 0,
            header_value_buffer: String::new(),
            header_value_cursor: 0,
            header_field: HeaderField::Key,
            editing_existing_header: None,
            selected_header_index: 0,
            header_autocomplete: None,
            response_view_mode: ResponseViewMode::Text,
            headers_scroll: 0,
            body_scroll: 0,
            events_scroll: 0,
            response_scroll: 0,
            url_h_scroll: 0,
            filter_h_scroll: 0,
            last_save_status: None,
            keymap: Keymap::default(),
            streamed_jq_available: false,
            streamed_jq_output_tx: None,
            streamed_jq_output_rx: None,
            streamed_jq_process: None,
        }
    }

    fn make_get(url: &str) -> HttpRequest {
        HttpRequest::new(HttpMethod::GET, url)
    }

    fn make_response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status_code: hyper::StatusCode::from_u16(status).unwrap(),
            headers: HashMap::new(),
            body: body.to_string(),
        }
    }

    // ── ResponseViewMode ──────────────────────────────────────────────────────

    #[test]
    fn test_response_view_mode_labels() {
        assert_eq!(ResponseViewMode::Text.label(), "text");
        assert_eq!(ResponseViewMode::Json.label(), "json");
    }

    // ── Request CRUD ──────────────────────────────────────────────────────────

    #[test]
    fn test_create_new_request_on_empty_list() {
        let mut app = app_with_requests(vec![]);
        assert_eq!(app.requests.len(), 0);
        assert_eq!(app.current_request_index, None);

        app.create_new_request();

        assert_eq!(app.requests.len(), 1);
        assert_eq!(app.current_request_index, Some(0));
        assert_eq!(app.requests[0].method, HttpMethod::GET);
        assert_eq!(&*app.requests[0].url, "https://");
    }

    #[test]
    fn test_create_new_request_appends_and_selects() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.create_new_request();
        assert_eq!(app.requests.len(), 2);
        assert_eq!(app.current_request_index, Some(1));
    }

    #[test]
    fn test_delete_only_request_clears_index() {
        let mut app = app_with_requests(vec![make_get("https://only.com")]);
        app.delete_current_request();
        assert!(app.requests.is_empty());
        assert_eq!(app.current_request_index, None);
    }

    #[test]
    fn test_delete_first_of_two_keeps_index_at_0() {
        let mut app = app_with_requests(vec![
            make_get("https://first.com"),
            make_get("https://second.com"),
        ]);
        app.current_request_index = Some(0);
        app.delete_current_request();
        assert_eq!(app.requests.len(), 1);
        assert_eq!(app.current_request_index, Some(0));
        assert_eq!(&*app.requests[0].url, "https://second.com");
    }

    #[test]
    fn test_delete_last_of_two_adjusts_index() {
        let mut app = app_with_requests(vec![
            make_get("https://first.com"),
            make_get("https://second.com"),
        ]);
        app.current_request_index = Some(1);
        app.delete_current_request();
        assert_eq!(app.requests.len(), 1);
        assert_eq!(app.current_request_index, Some(0));
    }

    #[test]
    fn test_delete_with_no_selection_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.current_request_index = None;
        app.delete_current_request();
        assert_eq!(app.requests.len(), 1);
    }

    // ── Request navigation ────────────────────────────────────────────────────

    #[test]
    fn test_select_next_request() {
        let mut app = app_with_requests(vec![
            make_get("https://a.com"),
            make_get("https://b.com"),
            make_get("https://c.com"),
        ]);
        app.current_request_index = Some(0);

        app.select_next_request();
        assert_eq!(app.current_request_index, Some(1));

        app.select_next_request();
        assert_eq!(app.current_request_index, Some(2));

        // At last item — should not advance
        app.select_next_request();
        assert_eq!(app.current_request_index, Some(2));
    }

    #[test]
    fn test_select_previous_request() {
        let mut app = app_with_requests(vec![make_get("https://a.com"), make_get("https://b.com")]);
        app.current_request_index = Some(1);

        app.select_previous_request();
        assert_eq!(app.current_request_index, Some(0));

        // Already at first — should not go negative
        app.select_previous_request();
        assert_eq!(app.current_request_index, Some(0));
    }

    #[test]
    fn test_select_next_on_empty_list_is_noop() {
        let mut app = app_with_requests(vec![]);
        app.select_next_request();
        assert_eq!(app.current_request_index, None);
    }

    #[test]
    fn test_select_previous_on_empty_list_is_noop() {
        let mut app = app_with_requests(vec![]);
        app.select_previous_request();
        assert_eq!(app.current_request_index, None);
    }

    #[test]
    fn test_get_current_request_none_when_no_selection() {
        let app = app_with_requests(vec![]);
        assert!(app.get_current_request().is_none());
    }

    #[test]
    fn test_get_current_request_returns_correct_item() {
        let app = app_with_requests(vec![
            make_get("https://first.com"),
            make_get("https://second.com"),
        ]);
        // index 0 is selected by default in app_with_requests
        assert_eq!(
            &*app.get_current_request().unwrap().url,
            "https://first.com"
        );
    }

    // ── Method cycling ────────────────────────────────────────────────────────

    #[test]
    fn test_toggle_method_full_cycle() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let expected_cycle = [
            HttpMethod::POST,
            HttpMethod::PUT,
            HttpMethod::PATCH,
            HttpMethod::DELETE,
            HttpMethod::HEAD,
            HttpMethod::OPTIONS,
            HttpMethod::GET, // wraps back
        ];
        for expected in &expected_cycle {
            app.toggle_method();
            assert_eq!(
                app.get_current_request().unwrap().method,
                *expected,
                "expected method {expected} after toggle"
            );
        }
    }

    #[test]
    fn test_toggle_method_no_request_is_noop() {
        let mut app = app_with_requests(vec![]);
        // Must not panic
        app.toggle_method();
    }

    // ── Focus cycling ─────────────────────────────────────────────────────────

    #[test]
    fn test_focus_next_field_full_cycle() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Url;

        let expected = [
            FocusableField::Headers,
            FocusableField::Body,
            FocusableField::Response,
            FocusableField::RequestEvents,
            FocusableField::Url, // wraps back
        ];
        for &field in &expected {
            app.focus_next_field();
            assert_eq!(app.focused_field, field);
        }
    }

    #[test]
    fn test_focus_previous_field_full_cycle() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Url;

        let expected = [
            FocusableField::RequestEvents,
            FocusableField::Response,
            FocusableField::Body,
            FocusableField::Headers,
            FocusableField::Url, // wraps back
        ];
        for &field in &expected {
            app.focus_previous_field();
            assert_eq!(app.focused_field, field);
        }
    }

    // ── Editing field entry ───────────────────────────────────────────────────

    #[test]
    fn test_edit_focused_url_populates_input_buffer() {
        let mut app = app_with_requests(vec![make_get("https://edit.me")]);
        app.focused_field = FocusableField::Url;
        app.edit_focused_field();
        assert_eq!(app.editing_field, Some(EditingField::Url));
        assert_eq!(app.input_buffer, "https://edit.me");
    }

    #[test]
    fn test_edit_focused_body_populates_input_buffer() {
        let req = HttpRequest::new(HttpMethod::POST, "https://a.com".to_string())
            .with_body("body content".to_string());
        let mut app = app_with_requests(vec![req]);
        app.focused_field = FocusableField::Body;
        app.edit_focused_field();
        assert_eq!(app.editing_field, Some(EditingField::Body));
        assert_eq!(app.input_buffer, "body content");
    }

    #[test]
    fn test_edit_focused_headers_clears_buffers_and_opens_autocomplete() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Headers;
        app.header_key_buffer = "leftover".to_string();
        app.edit_focused_field();
        assert_eq!(app.editing_field, Some(EditingField::Headers));
        assert_eq!(app.header_key_buffer, "");
        assert_eq!(app.header_value_buffer, "");
        assert!(app.header_autocomplete.is_some());
        assert_eq!(app.header_autocomplete.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn test_edit_focused_response_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Response;
        app.edit_focused_field();
        assert_eq!(app.editing_field, None);
    }

    #[test]
    fn test_edit_focused_events_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::RequestEvents;
        app.edit_focused_field();
        assert_eq!(app.editing_field, None);
    }

    // ── Scroll ────────────────────────────────────────────────────────────────

    #[test]
    fn test_scroll_down_increments_focused_field() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Body;
        app.scroll_down(1);
        assert_eq!(app.body_scroll, 1);
        app.scroll_down(2);
        assert_eq!(app.body_scroll, 3);
    }

    #[test]
    fn test_scroll_up_decrements_and_saturates_at_zero() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Response;
        app.response_scroll = 3;
        app.scroll_up(1);
        assert_eq!(app.response_scroll, 2);
        app.scroll_up(2);
        assert_eq!(app.response_scroll, 0);
        // Saturating — must not underflow
        app.scroll_up(1);
        assert_eq!(app.response_scroll, 0);
    }

    #[test]
    fn test_scroll_url_field_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Url;
        app.scroll_down(1);
        app.scroll_up(1);
        // No scroll state for Url — nothing to assert except no panic
    }

    #[test]
    fn test_scroll_per_field_independence() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);

        app.focused_field = FocusableField::Body;
        app.scroll_down(2);

        app.focused_field = FocusableField::RequestEvents;
        app.scroll_down(1);

        app.focused_field = FocusableField::Response;
        app.scroll_down(3);

        assert_eq!(app.body_scroll, 2);
        assert_eq!(app.events_scroll, 1);
        assert_eq!(app.response_scroll, 3);
        assert_eq!(app.headers_scroll, 0); // untouched
    }

    #[test]
    fn test_reset_request_screen_state() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.editing_field = Some(EditingField::Url);
        app.input_buffer = "something".to_string();
        app.header_key_buffer = "key".to_string();
        app.header_value_buffer = "val".to_string();
        app.focused_field = FocusableField::Body;
        app.headers_scroll = 5;
        app.body_scroll = 3;
        app.events_scroll = 2;
        app.response_scroll = 7;
        app.header_autocomplete = Some(AutocompleteState { selected: 4 });

        app.reset_request_screen_state();

        assert_eq!(app.editing_field, None);
        assert_eq!(app.input_buffer, "");
        assert_eq!(app.header_key_buffer, "");
        assert_eq!(app.header_value_buffer, "");
        assert_eq!(app.focused_field, FocusableField::Url);
        assert_eq!(app.headers_scroll, 0);
        assert_eq!(app.body_scroll, 0);
        assert_eq!(app.events_scroll, 0);
        assert_eq!(app.response_scroll, 0);
        assert!(app.header_autocomplete.is_none());
    }

    // ── Header CRUD ───────────────────────────────────────────────────────────

    #[test]
    fn test_select_next_header_wraps() {
        let req = HttpRequest::new(HttpMethod::GET, "https://a.com".to_string())
            .with_header("A".to_string(), "1".to_string())
            .with_header("B".to_string(), "2".to_string());
        let mut app = app_with_requests(vec![req]);
        app.selected_header_index = 0;

        app.select_next_header();
        assert_eq!(app.selected_header_index, 1);

        app.select_next_header();
        assert_eq!(app.selected_header_index, 0); // wrapped
    }

    #[test]
    fn test_select_previous_header_wraps() {
        let req = HttpRequest::new(HttpMethod::GET, "https://a.com".to_string())
            .with_header("A".to_string(), "1".to_string())
            .with_header("B".to_string(), "2".to_string());
        let mut app = app_with_requests(vec![req]);
        app.selected_header_index = 0;

        app.select_previous_header();
        assert_eq!(app.selected_header_index, 1); // wrapped to end
    }

    #[test]
    fn test_select_next_header_no_headers_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.selected_header_index = 0;
        app.select_next_header();
        assert_eq!(app.selected_header_index, 0);
    }

    #[test]
    fn test_delete_selected_header_removes_correct_header() {
        let req = HttpRequest::new(HttpMethod::GET, "https://a.com".to_string())
            .with_header("First".to_string(), "1".to_string())
            .with_header("Second".to_string(), "2".to_string());
        let mut app = app_with_requests(vec![req]);
        app.selected_header_index = 0;
        app.delete_selected_header();
        let remaining = &app.requests[0].headers;
        assert_eq!(remaining.len(), 1);
        assert!(!remaining.contains_key("First") || !remaining.contains_key("Second"));
    }

    // ── Autocomplete ──────────────────────────────────────────────────────────

    #[test]
    fn test_get_filtered_header_suggestions_empty_query_returns_all() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_field = HeaderField::Key;
        app.header_key_buffer = String::new();
        let suggestions = app.get_filtered_header_suggestions();
        // There are 76 standard headers — just verify we get a non-empty full list
        assert!(!suggestions.is_empty());
        assert!(suggestions.len() > 50);
    }

    #[test]
    fn test_get_filtered_header_suggestions_filters_by_prefix() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_field = HeaderField::Key;
        app.header_key_buffer = "content".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(!suggestions.is_empty());
        for s in &suggestions {
            assert!(
                s.to_lowercase().contains('c'),
                "expected 'content' match in: {s}"
            );
        }
        // All content-* headers should appear
        assert!(suggestions.iter().any(|s| *s == "content-type"));
        assert!(suggestions.iter().any(|s| *s == "content-length"));
    }

    #[test]
    fn test_get_filtered_header_suggestions_empty_when_not_editing_key() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_field = HeaderField::Value;
        app.header_key_buffer = "content".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_get_filtered_header_suggestions_sorted_prefix_first() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_field = HeaderField::Key;
        app.header_key_buffer = "con".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        // The first suggestions should start with "con"
        if let Some(first) = suggestions.first() {
            assert!(
                first.to_lowercase().starts_with("con"),
                "expected prefix-match first, got: {first}"
            );
        }
    }

    #[test]
    fn test_get_filtered_header_suggestions_no_match_returns_empty() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_field = HeaderField::Key;
        app.header_key_buffer = "zzzzzzzzzz".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_select_next_autocomplete_increments() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = Some(AutocompleteState { selected: 0 });
        app.select_next_autocomplete(5);
        assert_eq!(app.header_autocomplete.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn test_select_next_autocomplete_does_not_exceed_max() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = Some(AutocompleteState { selected: 4 });
        app.select_next_autocomplete(5);
        assert_eq!(app.header_autocomplete.as_ref().unwrap().selected, 4);
    }

    #[test]
    fn test_select_previous_autocomplete_decrements() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = Some(AutocompleteState { selected: 3 });
        app.select_previous_autocomplete();
        assert_eq!(app.header_autocomplete.as_ref().unwrap().selected, 2);
    }

    #[test]
    fn test_select_previous_autocomplete_does_not_underflow() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = Some(AutocompleteState { selected: 0 });
        app.select_previous_autocomplete();
        assert_eq!(app.header_autocomplete.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn test_apply_autocomplete_selection_sets_key_and_hides() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = Some(AutocompleteState { selected: 1 });
        let suggestions = &["content-type", "content-length", "authorization"];
        app.apply_autocomplete_selection(suggestions);
        assert_eq!(app.header_key_buffer, "content-length");
        assert_eq!(app.header_field, HeaderField::Value);
        assert!(app.header_autocomplete.is_none());
    }

    #[test]
    fn test_apply_autocomplete_selection_out_of_bounds_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = Some(AutocompleteState { selected: 99 });
        app.header_key_buffer = "original".to_string();
        let suggestions = &["accept"];
        app.apply_autocomplete_selection(suggestions);
        // Index 99 is out of bounds for a 1-element slice
        assert_eq!(app.header_key_buffer, "original");
    }

    // ── is_response_json / cycle_response_view_mode ───────────────────────────

    #[test]
    fn test_is_response_json_true_for_json_body() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Ok(make_response(200, r#"{"key":"value"}"#)));
        assert!(app.is_response_json());
    }

    #[test]
    fn test_is_response_json_false_for_plain_text() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Ok(make_response(200, "plain text response")));
        assert!(!app.is_response_json());
    }

    #[test]
    fn test_is_response_json_false_when_no_response() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        assert!(!app.is_response_json());
    }

    #[test]
    fn test_is_response_json_false_on_error_response() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Err(HttpError::RequestFailed {
            msg: "request failed".to_string(),
            source: None,
        }));
        assert!(!app.is_response_json());
    }

    #[test]
    fn test_cycle_response_view_mode_text_to_json_when_json_available() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Ok(make_response(200, r#"{"ok":true}"#)));
        app.response_view_mode = ResponseViewMode::Text;
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::Json);
    }

    #[test]
    fn test_cycle_response_view_mode_stays_text_when_not_json() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Ok(make_response(200, "not json")));
        app.response_view_mode = ResponseViewMode::Text;
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::Text);
    }

    #[test]
    fn test_cycle_response_view_mode_json_to_text() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Ok(make_response(200, r#"{"ok":true}"#)));
        app.response_view_mode = ResponseViewMode::Json;
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::Text);
    }

    // ── check_for_events ──────────────────────────────────────────────────────

    #[test]
    fn test_check_for_events_drains_channel() {
        use tokio::sync::mpsc;
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let (tx, rx) = mpsc::unbounded_channel();
        app.event_receiver = Some(rx);
        app.pending_request_index = Some(0);

        tx.send(crate::http_client::RequestEvent::Started).unwrap();
        tx.send(crate::http_client::RequestEvent::Completed(20))
            .unwrap();

        let received = app.check_for_events();
        assert!(received);
        let events = app.current_request_events();
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("started") || events[0].contains("Request"));
    }

    #[test]
    fn test_check_for_events_returns_false_when_empty() {
        use tokio::sync::mpsc;
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let (_tx, rx) = mpsc::unbounded_channel::<crate::http_client::RequestEvent>();
        app.event_receiver = Some(rx);

        let received = app.check_for_events();
        assert!(!received);
        assert!(app.current_request_events().is_empty());
    }

    #[test]
    fn test_check_for_events_returns_false_with_no_receiver() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.event_receiver = None;
        let received = app.check_for_events();
        assert!(!received);
    }

    // ── check_pending_response ────────────────────────────────────────────────

    #[test]
    fn test_check_pending_response_moves_result_to_last_response() {
        use tokio::sync::oneshot;
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let (tx, rx) = oneshot::channel::<Result<HttpResponse, crate::http_client::HttpError>>();
        app.pending_response = Some(rx);
        app.pending_request_index = Some(0);

        // Send the response before polling
        tx.send(Ok(make_response(200, "hello"))).unwrap();
        app.check_pending_response();

        assert!(app.current_last_response().is_some());
        assert!(app.pending_response.is_none());
        if let Some(Ok(resp)) = app.current_last_response() {
            assert_eq!(resp.status_code.as_u16(), 200);
            assert_eq!(resp.body, "hello");
        } else {
            panic!("expected Ok response");
        }
    }

    #[test]
    fn test_check_pending_response_closed_channel_becomes_error() {
        use tokio::sync::oneshot;
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let (tx, rx) = oneshot::channel::<Result<HttpResponse, crate::http_client::HttpError>>();
        app.pending_response = Some(rx);
        app.pending_request_index = Some(0);

        drop(tx); // close without sending
        app.check_pending_response();

        assert!(app.pending_response.is_none());
        assert!(matches!(app.current_last_response(), Some(Err(_))));
    }

    #[test]
    fn test_check_pending_response_empty_channel_keeps_pending() {
        use tokio::sync::oneshot;
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let (_tx, rx) = oneshot::channel::<Result<HttpResponse, crate::http_client::HttpError>>();
        app.pending_response = Some(rx);
        app.pending_request_index = Some(0);

        app.check_pending_response();

        // Still pending, nothing received yet
        assert!(app.pending_response.is_some());
        assert!(app.current_last_response().is_none());
    }

    // ── Persistence helpers ───────────────────────────────────────────────────

    #[test]
    fn test_save_and_load_requests_round_trip() {
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let base = dir.path();

        let requests = vec![
            HttpRequest::new(HttpMethod::GET, "https://save-test.com".to_string()),
            HttpRequest::new(HttpMethod::POST, "https://post-test.com".to_string())
                .with_body("payload".to_string()),
        ];

        save_requests_to_dir(&requests, Some(base)).expect("save failed");
        let loaded = load_requests_from_dir(Some(base)).expect("load failed");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].method, HttpMethod::GET);
        assert_eq!(&*loaded[0].url, "https://save-test.com");
        assert_eq!(loaded[1].method, HttpMethod::POST);
        assert_eq!(loaded[1].body, Some("payload".to_string()));
    }

    #[test]
    fn test_load_requests_returns_empty_when_file_absent() {
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let base = dir.path();

        let loaded = load_requests_from_dir(Some(base)).expect("should return Ok(vec![])");
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_save_requests_creates_parent_directories() {
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let base = dir.path();

        let requests = vec![make_get("https://mkdir-test.com")];
        let path = save_requests_to_dir(&requests, Some(base)).expect("save failed");
        assert!(path.exists(), "library file was not created");
    }

    #[test]
    fn test_save_requests_overwrites_on_second_call() {
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let base = dir.path();

        save_requests_to_dir(&[make_get("https://first.com")], Some(base)).expect("first save");
        save_requests_to_dir(
            &[
                make_get("https://second.com"),
                make_get("https://third.com"),
            ],
            Some(base),
        )
        .expect("second save");

        let loaded = load_requests_from_dir(Some(base)).expect("load");
        assert_eq!(loaded.len(), 2);
        assert_eq!(&*loaded[0].url, "https://second.com");
    }

    // ── fuzzy_match (module-private, tested via get_filtered_header_suggestions)

    #[test]
    fn test_fuzzy_match_via_suggestions_subsequence() {
        // "ct" should match "content-type" (c…t subsequence)
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_field = HeaderField::Key;
        app.header_key_buffer = "ct".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(
            suggestions.iter().any(|s| *s == "content-type"),
            "expected content-type in fuzzy results for 'ct'"
        );
    }

    #[test]
    fn test_fuzzy_match_via_suggestions_no_match() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_field = HeaderField::Key;
        app.header_key_buffer = "xyz_not_a_header_xyz".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(suggestions.is_empty());
    }

    // ── fuzzy_match (direct) ────────────────────────────────────────────────

    #[test]
    fn test_fuzzy_match_exact_match() {
        assert!(fuzzy_match("content-type", "content-type"));
    }

    #[test]
    fn test_fuzzy_match_subsequence() {
        assert!(fuzzy_match("ct", "content-type"));
    }

    #[test]
    fn test_fuzzy_match_case_sensitive() {
        // fuzzy_match is case-sensitive by design
        assert!(!fuzzy_match("CONTENT", "content-type"));
    }

    #[test]
    fn test_fuzzy_match_no_match() {
        assert!(!fuzzy_match("xyz", "content-type"));
    }

    #[test]
    fn test_fuzzy_match_empty_query_matches_anything() {
        assert!(fuzzy_match("", "anything"));
    }

    // ── word_boundary_before ─────────────────────────────────────────────────

    #[test]
    fn test_word_boundary_before_from_zero() {
        assert_eq!(word_boundary_before("hello world", 0), 0);
    }

    #[test]
    fn test_word_boundary_before_jumps_to_word_start() {
        // "hello world" — cursor at 6 (space), before should be at 0 ("hello" start)
        assert_eq!(word_boundary_before("hello world", 6), 0);
    }

    #[test]
    fn test_word_boundary_before_in_middle_of_word() {
        // cursor at 3 ("l" in "hello") — before should be at 0
        assert_eq!(word_boundary_before("hello world", 3), 0);
    }

    #[test]
    fn test_word_boundary_before_non_alnum() {
        // "abc://def" — cursor at 6 ("/" in "://"), before should skip "://" to 3 ("abc" end)
        assert_eq!(word_boundary_before("abc://def", 6), 3);
    }

    #[test]
    fn test_word_boundary_before_skip_leading_whitespace() {
        // "  hello" — cursor at 6 (after "hello"), should skip whitespace to "hello" start at 2
        assert_eq!(word_boundary_before("  hello", 6), 2);
    }

    #[test]
    fn test_word_boundary_before_already_at_start_of_word() {
        // cursor at 6, right at "world" start
        assert_eq!(word_boundary_before("hello world", 6), 0);
    }

    // ── word_boundary_after ──────────────────────────────────────────────────

    #[test]
    fn test_word_boundary_after_from_end() {
        assert_eq!(word_boundary_after("hello world", 11), 11);
    }

    #[test]
    fn test_word_boundary_after_jumps_to_next_word() {
        // cursor at 0 ("h" in "hello") — after should be at 5 (end of "hello")
        assert_eq!(word_boundary_after("hello world", 0), 5);
    }

    #[test]
    fn test_word_boundary_after_in_middle_of_word() {
        // cursor at 2 ("l" in "hello") — after should be at 5
        assert_eq!(word_boundary_after("hello world", 2), 5);
    }

    #[test]
    fn test_word_boundary_after_non_alnum() {
        // "abc://def" — cursor at 3 ('c'), after should jump to index 6 (start of "def")
        assert_eq!(word_boundary_after("abc://def", 3), 6);
    }

    #[test]
    fn test_word_boundary_after_skip_whitespace() {
        // cursor at 5 (space), after should skip to "world" end at 11
        assert_eq!(word_boundary_after("hello world", 5), 11);
    }

    // ── strip_line ───────────────────────────────────────────────────────────

    #[test]
    fn test_strip_line_with_prefix_suffix() {
        let prefix = RegexPattern::new(r"^\w+:\s*".to_string());
        let suffix = RegexPattern::new(r"\s*$".to_string());
        let result = strip_line("data: {\"key\": \"value\"}  ", &Some(prefix), &Some(suffix));
        assert_eq!(result.unwrap(), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_strip_line_prefix_only() {
        let prefix = RegexPattern::new(r"^event:\s*".to_string());
        let suffix = RegexPattern::new(r"^$".to_string());
        let result = strip_line("event: update", &Some(prefix), &Some(suffix));
        assert_eq!(result.unwrap(), "update");
    }

    #[test]
    fn test_strip_line_no_change() {
        let prefix = RegexPattern::new(r"^$".to_string());
        let suffix = RegexPattern::new(r"^$".to_string());
        let result = strip_line("plain line", &Some(prefix), &Some(suffix));
        assert_eq!(result.unwrap(), "plain line");
    }

    #[test]
    fn test_strip_line_no_regex_available_with_some() {
        let prefix = RegexPattern::new(r"[invalid".to_string());
        let noop_suffix = RegexPattern::new(r"^$".to_string());
        let result = strip_line("test", &Some(prefix), &Some(noop_suffix));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid prefix regex"));
    }

    // ── current_request_is_pending ───────────────────────────────────────────

    #[test]
    fn test_current_request_is_pending_true() {
        use tokio::sync::oneshot;
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let (_tx, rx) = oneshot::channel::<Result<HttpResponse, crate::http_client::HttpError>>();
        app.pending_response = Some(rx);
        app.pending_request_index = Some(0);
        assert!(app.current_request_is_pending());
    }

    #[test]
    fn test_current_request_is_pending_false_no_request() {
        let app = app_with_requests(vec![]);
        assert!(!app.current_request_is_pending());
    }

    #[test]
    fn test_current_request_is_pending_false_no_pending() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        assert!(!app.current_request_is_pending());
    }

    #[test]
    fn test_current_request_is_pending_false_mismatched_index() {
        use tokio::sync::oneshot;
        let mut app = app_with_requests(vec![make_get("https://a.com"), make_get("https://b.com")]);
        let (_tx, rx) = oneshot::channel::<Result<HttpResponse, crate::http_client::HttpError>>();
        app.pending_response = Some(rx);
        app.pending_request_index = Some(1); // different from current (0)
        assert!(!app.current_request_is_pending());
    }

    // ── Accessors: current_jq_filter, stream_prefix/suffix, streamed_body ────

    #[test]
    fn test_current_jq_filter_default() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        assert_eq!(app.current_jq_filter(), ".");
    }

    #[test]
    fn test_current_jq_filter_custom() {
        let mut req = make_get("https://a.com");
        req.jq_filter = crate::http_client::JqFilter::from(".key");
        let app = app_with_requests(vec![req]);
        assert_eq!(app.current_jq_filter(), ".key");
    }

    #[test]
    fn test_current_jq_filter_no_request() {
        let app = app_with_requests(vec![]);
        assert_eq!(app.current_jq_filter(), ".");
    }

    #[test]
    fn test_current_stream_prefix_regex_default() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        assert_eq!(app.current_stream_prefix_regex(), r"^\w+:\s*");
    }

    #[test]
    fn test_current_stream_prefix_regex_no_request() {
        let app = app_with_requests(vec![]);
        assert_eq!(app.current_stream_prefix_regex(), r"^\w+:\s*");
    }

    #[test]
    fn test_current_stream_suffix_regex_default() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        assert_eq!(app.current_stream_suffix_regex(), r"\s*$");
    }

    #[test]
    fn test_current_stream_suffix_regex_no_request() {
        let app = app_with_requests(vec![]);
        assert_eq!(app.current_stream_suffix_regex(), r"\s*$");
    }

    #[test]
    fn test_current_streamed_body_returns_body() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .streamed_body = "response body".to_string();
        assert_eq!(app.current_streamed_body(), "response body");
    }

    #[test]
    fn test_current_streamed_body_no_request() {
        let app = app_with_requests(vec![]);
        assert_eq!(app.current_streamed_body(), "");
    }

    #[test]
    fn test_current_streamed_body_no_state() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        assert_eq!(app.current_streamed_body(), "");
    }

    // ── focused_scroll (tested via scroll_up/scroll_down per field) ──────────

    #[test]
    fn test_scroll_headers_independence() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::Headers;
        app.scroll_down(5);
        assert_eq!(app.headers_scroll, 5);
        app.scroll_up(2);
        assert_eq!(app.headers_scroll, 3);
    }

    #[test]
    fn test_scroll_events_independence() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.focused_field = FocusableField::RequestEvents;
        app.scroll_down(7);
        assert_eq!(app.events_scroll, 7);
        app.scroll_up(3);
        assert_eq!(app.events_scroll, 4);
    }

    // ── key_context ──────────────────────────────────────────────────────────

    #[test]
    fn test_key_context_main_screen() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        let ctx = app.key_context();
        assert_eq!(ctx.screen, CurrentScreen::Main);
        assert_eq!(ctx.editing, None);
        assert_eq!(ctx.focus, FocusableField::Url);
    }

    #[test]
    fn test_key_context_request_screen_editing_url() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.current_screen = CurrentScreen::Request;
        app.editing_field = Some(EditingField::Url);
        let ctx = app.key_context();
        assert_eq!(ctx.screen, CurrentScreen::Request);
        assert_eq!(ctx.editing, Some(EditingField::Url));
    }

    // ── edit_selected_header ─────────────────────────────────────────────────

    #[test]
    fn test_edit_selected_header_populates_buffers() {
        let req = HttpRequest::new(HttpMethod::GET, "https://a.com")
            .with_header("Content-Type", "application/json");
        let mut app = app_with_requests(vec![req]);
        app.selected_header_index = 0;
        app.edit_selected_header();
        assert_eq!(app.editing_field, Some(EditingField::Headers));
        assert_eq!(app.header_key_buffer, "Content-Type");
        assert_eq!(app.header_value_buffer, "application/json");
        assert_eq!(app.header_field, HeaderField::Key);
        assert!(app.header_autocomplete.is_some());
    }

    #[test]
    fn test_edit_selected_header_no_headers_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.selected_header_index = 0;
        app.edit_selected_header();
        assert_eq!(app.editing_field, None);
    }

    #[test]
    fn test_edit_selected_header_out_of_bounds_is_noop() {
        let req = HttpRequest::new(HttpMethod::GET, "https://a.com").with_header("X-One", "1");
        let mut app = app_with_requests(vec![req]);
        app.selected_header_index = 5; // out of bounds
        app.edit_selected_header();
        assert_eq!(app.editing_field, None);
    }

    // ── cycle_response_view_mode with StreamedJson ───────────────────────────

    #[test]
    fn test_cycle_to_streamed_json() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Ok(make_response(200, "plain text")));
        app.streamed_jq_available = true;
        app.response_view_mode = ResponseViewMode::Text;
        // Text -> StreamedJson (not json but streamed available)
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::StreamedJson);
    }

    #[test]
    fn test_cycle_from_json_to_streamed() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.per_request
            .entry(0)
            .or_insert_with(PerRequestState::new)
            .last_response = Some(Ok(make_response(200, r#"{"ok":true}"#)));
        app.streamed_jq_available = true;
        app.response_view_mode = ResponseViewMode::Json;
        // Json -> StreamedJson
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::StreamedJson);
    }

    #[test]
    fn test_cycle_from_streamed_to_text() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.response_view_mode = ResponseViewMode::StreamedJson;
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::Text);
    }

    // ── Autocomplete with None ───────────────────────────────────────────────

    #[test]
    fn test_select_next_autocomplete_when_none_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = None;
        app.select_next_autocomplete(5);
        assert!(app.header_autocomplete.is_none());
    }

    #[test]
    fn test_select_previous_autocomplete_when_none_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete = None;
        app.select_previous_autocomplete();
        assert!(app.header_autocomplete.is_none());
    }

    // ── save_requests ────────────────────────────────────────────────────────

    #[test]
    fn test_save_requests_sets_success_status() {
        use tempfile::TempDir;

        let app = app_with_requests(vec![make_get("https://save-status.com")]);
        let dir = TempDir::new().expect("tempdir");
        // Override the save target via save_requests_to_dir
        let result = crate::app::save_requests_to_dir(&app.requests, Some(dir.path()));
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_save_requests_empty_creates_file() {
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let result = crate::app::save_requests_to_dir(&[], Some(dir.path()));
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
        // Verify content is an empty array
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.trim(), "[]");
    }

    // ── current_streamed_jq_output ───────────────────────────────────────────

    #[test]
    fn test_current_streamed_jq_output_empty() {
        let app = app_with_requests(vec![make_get("https://a.com")]);
        assert_eq!(app.current_streamed_jq_output(), "");
    }

    #[test]
    fn test_current_streamed_jq_output_joins_lines() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        let state = app
            .per_request
            .entry(0)
            .or_insert_with(PerRequestState::new);
        state.jq_output.push("line1".to_string());
        state.jq_output.push("line2".to_string());
        assert_eq!(app.current_streamed_jq_output(), "line1\nline2");
    }

    #[test]
    fn test_current_streamed_jq_output_no_request() {
        let app = app_with_requests(vec![]);
        assert_eq!(app.current_streamed_jq_output(), "");
    }

    // ── JqProcess::drop (no panic) ───────────────────────────────────────────

    #[test]
    fn test_jq_process_drop_exited_child_does_not_panic() {
        // Spawn a process that exits immediately, wrap in JqProcess, then drop
        let mut child = std::process::Command::new("true")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn true");
        let stdin = child.stdin.take().expect("stdin");
        // Wait for the process to exit before dropping
        // (drop should handle already-exited children gracefully)
        let jq = JqProcess { stdin, child };
        drop(jq); // must not panic
    }
}
