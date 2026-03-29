use crate::http_client::{HttpRequest, HttpResponse, HttpRuntime, RequestEvent};
use crate::keymap::{KeyContext, Keymap};
use hyper::Method;
use regex::Regex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

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

pub struct App {
    pub current_screen: CurrentScreen,
    pub requests: Vec<HttpRequest>,
    pub current_request_index: Option<usize>,
    pub pending_response:
        Option<oneshot::Receiver<Result<HttpResponse, crate::http_client::HttpError>>>,
    /// The request index that is currently in-flight (set when the request is sent).
    pub pending_request_index: Option<usize>,
    pub event_receiver: Option<mpsc::UnboundedReceiver<RequestEvent>>,
    /// Per-request event log (keyed by request index).
    pub request_events_per: HashMap<usize, Vec<String>>,
    /// Per-request streamed body accumulator (keyed by request index).
    pub streamed_body_per: HashMap<usize, String>,
    /// Per-request last response (keyed by request index).
    pub last_response_per: HashMap<usize, Result<HttpResponse, String>>,
    pub http_runtime: HttpRuntime,
    pub editing_field: Option<EditingField>,
    pub focused_field: FocusableField,
    pub input_buffer: String,
    pub header_key_buffer: String,
    pub header_value_buffer: String,
    pub editing_header_key: bool,
    pub editing_existing_header: Option<String>, // Original header key when editing
    pub selected_header_index: usize,
    // Autocomplete state for headers
    pub header_autocomplete_visible: bool,
    pub header_autocomplete_selected: usize,
    // Response view mode
    pub response_view_mode: ResponseViewMode,
    // Scroll positions for each field
    pub headers_scroll: u16,
    pub body_scroll: u16,
    pub events_scroll: u16,
    pub response_scroll: u16,
    // Last save status message (shown briefly in the footer)
    pub last_save_status: Option<String>,
    // Keybinding map (single source of truth for dispatch + hints)
    pub keymap: Keymap,

    // ── Streamed-jq mode ──────────────────────────────────────────────────────
    /// Whether at least one stripped line successfully parsed as JSON —
    /// gates availability of StreamedJson mode in the view-mode cycle.
    pub streamed_jq_available: bool,
    /// Per-request accumulator of jq-filtered lines (as raw ANSI strings).
    pub streamed_jq_output_per: HashMap<usize, Vec<String>>,
    /// Per-request line reassembly buffer (holds incomplete last line of a chunk).
    pub streamed_line_buffer_per: HashMap<usize, String>,
    /// Per-request accumulator of stripped (prefix/suffix removed) lines,
    /// used to re-feed jq when the filter or regexes change.
    pub streamed_stripped_lines_per: HashMap<usize, Vec<String>>,
    /// Sender for jq events coming from the reader threads.
    pub streamed_jq_output_tx: Option<mpsc::UnboundedSender<JqEvent>>,
    /// Receiver for jq events from the reader threads.
    pub streamed_jq_output_rx: Option<mpsc::UnboundedReceiver<JqEvent>>,
    /// Per-request: the last stripped line fed to jq, used to prepend the
    /// original value when jq reports a parse error for it.
    pub streamed_jq_last_fed_per: HashMap<usize, String>,
    /// Handle for the jq subprocess stdin (kept alive so the process stays running).
    pub streamed_jq_stdin: Option<std::process::ChildStdin>,
    /// Handle to the jq child process (for killing/waiting on cleanup).
    pub streamed_jq_child: Option<std::process::Child>,
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
            request_events_per: HashMap::new(),
            last_response_per: HashMap::new(),
            http_runtime: HttpRuntime::new().expect("Failed to create HTTP runtime"),
            editing_field: None,
            focused_field: FocusableField::Url,
            input_buffer: String::new(),
            header_key_buffer: String::new(),
            header_value_buffer: String::new(),
            editing_header_key: true,
            editing_existing_header: None,
            selected_header_index: 0,
            header_autocomplete_visible: false,
            header_autocomplete_selected: 0,
            response_view_mode: ResponseViewMode::Text,
            headers_scroll: 0,
            body_scroll: 0,
            events_scroll: 0,
            response_scroll: 0,
            streamed_body_per: HashMap::new(),
            last_save_status: None,
            keymap: Keymap::default(),
            streamed_jq_available: false,
            streamed_jq_output_per: HashMap::new(),
            streamed_line_buffer_per: HashMap::new(),
            streamed_stripped_lines_per: HashMap::new(),
            streamed_jq_output_tx: None,
            streamed_jq_output_rx: None,
            streamed_jq_last_fed_per: HashMap::new(),
            streamed_jq_stdin: None,
            streamed_jq_child: None,
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
            .map(|r| r.jq_filter.as_str())
            .unwrap_or(".")
    }

    pub fn current_stream_prefix_regex(&self) -> &str {
        self.get_current_request()
            .map(|r| r.stream_prefix_regex.as_str())
            .unwrap_or(r"^\w+:\s*")
    }

    pub fn current_stream_suffix_regex(&self) -> &str {
        self.get_current_request()
            .map(|r| r.stream_suffix_regex.as_str())
            .unwrap_or(r"\s*$")
    }

    pub fn create_new_request(&mut self) {
        let new_request = HttpRequest::new(Method::GET, "https://".to_string());
        self.requests.push(new_request);
        self.current_request_index = Some(self.requests.len() - 1);
    }

    pub fn delete_current_request(&mut self) {
        if let Some(idx) = self.current_request_index {
            if idx < self.requests.len() {
                self.requests.remove(idx);
                // Clean up per-request response state for the deleted index.
                // Indices above the deleted one shift down by one, so rebuild the maps.
                self.last_response_per = self
                    .last_response_per
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
                self.streamed_body_per = self
                    .streamed_body_per
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
                self.request_events_per = self
                    .request_events_per
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
                // Also shift the pending index if needed.
                if let Some(pending) = self.pending_request_index {
                    if pending == idx {
                        // The in-flight request was deleted; abandon its results.
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
                // Kill any existing streamed-jq process
                self.kill_streamed_jq();

                let (result_rx, event_rx) = self.http_runtime.execute_request(request);
                self.pending_response = Some(result_rx);
                self.pending_request_index = Some(idx);
                self.event_receiver = Some(event_rx);
                self.request_events_per.insert(idx, Vec::new());
                self.streamed_body_per.insert(idx, String::new());
                self.last_response_per.remove(&idx);
                // Reset per-request streamed-jq accumulators
                self.streamed_jq_output_per.insert(idx, Vec::new());
                self.streamed_line_buffer_per.insert(idx, String::new());
                self.streamed_stripped_lines_per.insert(idx, Vec::new());
                self.streamed_jq_last_fed_per.remove(&idx);
                self.streamed_jq_available = false;
            }
        }
    }

    pub fn check_pending_response(&mut self) {
        if let Some(receiver) = &mut self.pending_response {
            match receiver.try_recv() {
                Ok(result) => {
                    if let Some(idx) = self.pending_request_index {
                        match result {
                            Ok(resp) => {
                                self.streamed_body_per.insert(idx, resp.body.clone());
                                self.last_response_per.insert(idx, Ok(resp));
                            }
                            Err(e) => {
                                self.last_response_per.insert(idx, Err(e.to_string()));
                            }
                        }
                    }
                    self.pending_response = None;
                    // NOTE: We intentionally do NOT clear pending_request_index,
                    // flush the line buffer, or close jq stdin here.  Body-chunk
                    // events may still be buffered in event_receiver — they are
                    // processed in check_for_events using pending_request_index.
                    // Cleanup happens in check_for_events once the event channel
                    // is fully drained (sender dropped / disconnected).
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still waiting
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    if let Some(idx) = self.pending_request_index {
                        self.last_response_per
                            .insert(idx, Err("Request channel closed".to_string()));
                    }
                    self.pending_response = None;
                    self.pending_request_index = None;
                    self.streamed_jq_stdin = None;
                }
            }
        }
    }

    pub fn check_for_events(&mut self) -> bool {
        let mut received_any = false;

        // Drain jq output events from the reader threads.
        // Also detect if the jq channel is disconnected (jq exited,
        // all reader threads dropped their tx clones).
        let mut jq_channel_closed = false;
        if let Some(rx) = &mut self.streamed_jq_output_rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => {
                        if let Some(idx) = self.current_request_index {
                            let output_lines = self.streamed_jq_output_per.entry(idx).or_default();
                            match event {
                                JqEvent::Output(line) => {
                                    output_lines.push(line);
                                }
                                JqEvent::Error(err) => {
                                    let original = self
                                        .streamed_jq_last_fed_per
                                        .get(&idx)
                                        .cloned()
                                        .unwrap_or_default();
                                    output_lines
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
            // jq has exited and all output has been consumed; clean up.
            self.streamed_jq_output_rx = None;
            self.streamed_jq_output_tx = None;
            if let Some(mut child) = self.streamed_jq_child.take() {
                let _ = child.wait();
            }
        }

        // Collect all events first (releases the borrow on event_receiver), then process.
        // Also detect whether the event channel is disconnected (sender dropped,
        // all messages already received) — this means no more body chunks will arrive.
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
                match event {
                    crate::http_client::RequestEvent::BodyChunk(s) => {
                        self.streamed_body_per.entry(idx).or_default().push_str(&s);
                        self.request_events_per
                            .entry(idx)
                            .or_default()
                            .push(format!("Response chunk received: {} bytes", s.len()));
                        // Feed the chunk into the line reassembly buffer
                        self.process_chunk(idx, s);
                    }
                    other => {
                        self.request_events_per
                            .entry(idx)
                            .or_default()
                            .push(other.to_string());
                    }
                }
            }
        }

        // Once the HTTP response oneshot has resolved (pending_response is None)
        // AND the event channel is fully drained+closed, perform final cleanup:
        // flush the line buffer and close jq stdin so it can process the last input.
        if event_channel_closed && self.pending_response.is_none() {
            if let Some(idx) = self.pending_request_index.take() {
                self.flush_line_buffer(idx);
            }
            self.streamed_jq_stdin = None;
            self.event_receiver = None;
        }

        received_any
    }

    pub fn toggle_method(&mut self) {
        if let Some(request) = self.get_current_request_mut() {
            // Cycle through standard HTTP methods
            request.method = match request.method {
                Method::GET => Method::POST,
                Method::POST => Method::PUT,
                Method::PUT => Method::PATCH,
                Method::PATCH => Method::DELETE,
                Method::DELETE => Method::HEAD,
                Method::HEAD => Method::OPTIONS,
                Method::OPTIONS => Method::GET,
                // For any other methods (custom), cycle back to GET
                _ => Method::GET,
            };
        }
    }

    /// Returns whether a request is currently in-flight for the *current* request.
    pub fn current_request_is_pending(&self) -> bool {
        self.pending_request_index == self.current_request_index && self.pending_response.is_some()
    }

    /// Returns the last response for the currently selected request, if any.
    pub fn current_last_response(&self) -> Option<&Result<HttpResponse, String>> {
        self.current_request_index
            .and_then(|idx| self.last_response_per.get(&idx))
    }

    /// Returns the streamed body accumulator for the currently selected request.
    pub fn current_streamed_body(&self) -> &str {
        self.current_request_index
            .and_then(|idx| self.streamed_body_per.get(&idx))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Returns the request events log for the currently selected request.
    pub fn current_request_events(&self) -> &[String] {
        self.current_request_index
            .and_then(|idx| self.request_events_per.get(&idx))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Returns true when the last response body is valid JSON (parsed by serde_json).
    pub fn is_response_json(&self) -> bool {
        if let Some(Ok(resp)) = self.current_last_response() {
            serde_json::from_str::<serde_json::Value>(&resp.body).is_ok()
        } else {
            false
        }
    }

    // ── Streamed-jq helpers ───────────────────────────────────────────────────

    /// Strip prefix and suffix regex from a raw line.  Returns the stripped
    /// string, or an error message if a regex fails to compile.
    fn strip_line(&self, raw: &str) -> Result<String, String> {
        let stripped = if let Ok(re) = Regex::new(self.current_stream_prefix_regex()) {
            re.replace(raw, "").into_owned()
        } else {
            return Err(format!(
                "Invalid prefix regex: {}",
                self.current_stream_prefix_regex()
            ));
        };
        let stripped = if let Ok(re) = Regex::new(self.current_stream_suffix_regex()) {
            re.replace(&stripped, "").into_owned()
        } else {
            return Err(format!(
                "Invalid suffix regex: {}",
                self.current_stream_suffix_regex()
            ));
        };
        Ok(stripped)
    }

    /// Ensure the persistent jq subprocess is running (starting it if needed).
    /// Returns `true` if jq is ready to receive input.
    fn ensure_jq_running(&mut self) -> bool {
        if self.streamed_jq_stdin.is_some() {
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
                self.streamed_jq_stdin = Some(stdin);
                self.streamed_jq_child = Some(child);
                true
            }
            Err(e) => {
                // Record the error as a fake output line
                let msg = format!("\x1b[31m// Failed to start jq: {e}\x1b[0m");
                if let Some(idx) = self.current_request_index {
                    self.streamed_jq_output_per
                        .entry(idx)
                        .or_default()
                        .push(msg);
                }
                false
            }
        }
    }

    /// Kill and clean up the jq subprocess if running.
    pub fn kill_streamed_jq(&mut self) {
        self.streamed_jq_stdin = None; // drops stdin → jq gets EOF
        self.streamed_jq_output_tx = None;
        self.streamed_jq_output_rx = None;
        if let Some(mut child) = self.streamed_jq_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Send a single stripped line to the jq subprocess stdin.
    /// If the write fails (jq died after a parse error), we drain any remaining
    /// output from the old process, kill it, spawn a fresh one, and retry.
    fn feed_line_to_jq(&mut self, idx: usize, stripped: &str) {
        if !self.ensure_jq_running() {
            return;
        }
        let write_ok = if let Some(stdin) = &mut self.streamed_jq_stdin {
            writeln!(stdin, "{stripped}").is_ok()
        } else {
            false
        };
        if !write_ok {
            // jq died (parse error on previous line). Drain any output that
            // already arrived in the channel before dropping it, then restart.
            if let Some(rx) = &mut self.streamed_jq_output_rx {
                while let Ok(event) = rx.try_recv() {
                    let out = self.streamed_jq_output_per.entry(idx).or_default();
                    match event {
                        JqEvent::Output(line) => out.push(line),
                        JqEvent::Error(err) => {
                            let original = self
                                .streamed_jq_last_fed_per
                                .get(&idx)
                                .cloned()
                                .unwrap_or_default();
                            out.push(format!("{original} \x1b[31m// jq: {err}\x1b[0m"));
                        }
                    }
                }
            }
            self.kill_streamed_jq();
            if self.ensure_jq_running() {
                if let Some(stdin) = &mut self.streamed_jq_stdin {
                    let _ = writeln!(stdin, "{stripped}");
                }
            }
        }
        // Remember this as the last fed line so we can show it alongside
        // any jq parse error that arrives for it.
        self.streamed_jq_last_fed_per
            .insert(idx, stripped.to_string());
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

    /// Process a raw chunk: append to line buffer, extract complete lines,
    /// apply prefix/suffix stripping, update availability flag, and feed jq.
    fn process_chunk(&mut self, idx: usize, chunk: String) {
        let buf = self.streamed_line_buffer_per.entry(idx).or_default();
        buf.push_str(&chunk);

        // Split on newlines; the last element (possibly empty) is the
        // incomplete tail that stays in the buffer.
        let combined = std::mem::take(buf);
        let mut parts: Vec<&str> = combined.split('\n').collect();
        let tail = parts.pop().unwrap_or("").to_string();
        *self.streamed_line_buffer_per.entry(idx).or_default() = tail;

        for raw_line in parts {
            self.handle_complete_line(idx, raw_line);
        }
    }

    /// Process one complete (newline-terminated) raw line.
    fn handle_complete_line(&mut self, idx: usize, raw: &str) {
        let trimmed = raw.trim_end_matches('\r'); // handle CRLF
        match self.strip_line(trimmed) {
            Err(e) => {
                let msg = format!("\x1b[31m// {e}\x1b[0m");
                self.streamed_jq_output_per
                    .entry(idx)
                    .or_default()
                    .push(msg);
            }
            Ok(stripped) => {
                if stripped.is_empty() {
                    return;
                }
                // Check if this line is valid JSON → unlock streamed-jq mode
                if serde_json::from_str::<serde_json::Value>(&stripped).is_ok() {
                    self.streamed_jq_available = true;
                }
                // Save stripped line for re-processing on filter change
                self.streamed_stripped_lines_per
                    .entry(idx)
                    .or_default()
                    .push(stripped.clone());
                // Feed to jq
                self.feed_line_to_jq(idx, &stripped);
            }
        }
    }

    /// Flush any remaining bytes in the line buffer as a final (incomplete) line.
    fn flush_line_buffer(&mut self, idx: usize) {
        let tail = self
            .streamed_line_buffer_per
            .get(&idx)
            .cloned()
            .unwrap_or_default();
        if !tail.is_empty() {
            self.handle_complete_line(idx, &tail);
            self.streamed_line_buffer_per.insert(idx, String::new());
        }
    }

    /// Re-process all accumulated stripped lines through a fresh jq subprocess
    /// (called after the user changes the jq filter or prefix/suffix regexes).
    ///
    /// Unlike the live-streaming path this runs jq **synchronously** — one
    /// jq invocation per stripped line, waiting for each to finish before
    /// moving on.  This guarantees that no output is lost to timing races
    /// (jq exiting before we drain its channel, etc.).
    pub fn reprocess_streamed_jq(&mut self) {
        let Some(idx) = self.current_request_index else {
            return;
        };
        // Kill any running live-streaming jq process.
        self.kill_streamed_jq();

        // Clear old output and last-fed tracker.
        self.streamed_jq_output_per.insert(idx, Vec::new());
        self.streamed_jq_last_fed_per.remove(&idx);

        // Re-strip all raw lines from the complete body, rebuilding the
        // stripped-lines accumulator and the availability flag.
        let body = self
            .streamed_body_per
            .get(&idx)
            .cloned()
            .unwrap_or_default();
        self.streamed_stripped_lines_per.insert(idx, Vec::new());
        self.streamed_jq_available = false;

        // Collect stripped lines without calling handle_complete_line
        // (which would try to feed the async jq that we just killed).
        let raw_lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
        let mut stripped_lines: Vec<String> = Vec::new();
        for raw in &raw_lines {
            let trimmed = raw.trim_end_matches('\r');
            match self.strip_line(trimmed) {
                Err(e) => {
                    // Invalid regex — record error and continue.
                    self.streamed_jq_output_per
                        .entry(idx)
                        .or_default()
                        .push(format!("\x1b[31m// {e}\x1b[0m"));
                }
                Ok(stripped) => {
                    if stripped.is_empty() {
                        continue;
                    }
                    if serde_json::from_str::<serde_json::Value>(&stripped).is_ok() {
                        self.streamed_jq_available = true;
                    }
                    self.streamed_stripped_lines_per
                        .entry(idx)
                        .or_default()
                        .push(stripped.clone());
                    stripped_lines.push(stripped);
                }
            }
        }

        // Run jq synchronously on the collected stripped lines.
        let filter = self.current_jq_filter().to_string();
        let batch_output = Self::run_jq_batch_sync(&filter, &stripped_lines);
        self.streamed_jq_output_per
            .entry(idx)
            .or_default()
            .extend(batch_output);
    }

    /// Returns the joined streamed-jq output for the currently selected request.
    pub fn current_streamed_jq_output(&self) -> String {
        self.current_request_index
            .and_then(|idx| self.streamed_jq_output_per.get(&idx))
            .map(|lines| lines.join("\n"))
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

    /// Runs the current `jq_filter` against the last response body via the
    /// `jq` binary and returns the colourised output, or an error string.
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
                    self.input_buffer = request.url.clone();
                }
                EditingField::Url
            }
            FocusableField::Headers => {
                // Start adding a new header
                self.header_key_buffer.clear();
                self.header_value_buffer.clear();
                self.editing_header_key = true;
                self.editing_existing_header = None;
                self.header_autocomplete_visible = true;
                self.header_autocomplete_selected = 0;
                EditingField::Headers
            }
            FocusableField::Body => {
                if let Some(request) = self.get_current_request() {
                    self.input_buffer = request.body.clone().unwrap_or_default();
                }
                EditingField::Body
            }
            FocusableField::RequestEvents | FocusableField::Response => {
                // These fields are not editable
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
        // Get the header data first
        let header_data = self.get_current_request().and_then(|request| {
            request
                .headers
                .iter()
                .nth(self.selected_header_index)
                .map(|(k, v)| (k.clone(), v.clone()))
        });

        // Now set the buffers
        if let Some((key, value)) = header_data {
            self.editing_existing_header = Some(key.clone()); // Track original key
            self.header_key_buffer = key;
            self.header_value_buffer = value;
            self.editing_field = Some(EditingField::Headers);
            self.editing_header_key = true; // Start editing header name
            self.header_autocomplete_visible = true;
            self.header_autocomplete_selected = 0;
        }
    }

    pub fn delete_selected_header(&mut self) {
        let selected_idx = self.selected_header_index;

        if let Some(request) = self.get_current_request_mut() {
            let keys: Vec<_> = request.headers.keys().cloned().collect();
            if let Some(key) = keys.get(selected_idx) {
                request.remove_header(key);
                // Adjust selected index if needed
                let new_len = request.headers.len();
                if selected_idx >= new_len && selected_idx > 0 {
                    self.selected_header_index = selected_idx - 1;
                }
            }
        }
    }

    /// Returns a mutable reference to the scroll counter for the currently focused field,
    /// or `None` if the focused field is not scrollable (e.g. `Url`).
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

    /// Returns the current key context for keymap resolution and hint display.
    pub fn key_context(&self) -> KeyContext {
        KeyContext {
            screen: self.current_screen,
            editing: self.editing_field,
            focus: self.focused_field,
        }
    }

    pub fn reset_request_screen_state(&mut self) {
        // Reset editing state
        self.editing_field = None;
        self.input_buffer.clear();
        self.header_key_buffer.clear();
        self.header_value_buffer.clear();
        self.editing_header_key = true;
        self.editing_existing_header = None;

        // Reset focus
        self.focused_field = FocusableField::Url;

        // Reset scroll positions
        self.headers_scroll = 0;
        self.body_scroll = 0;
        self.events_scroll = 0;
        self.response_scroll = 0;

        // Reset autocomplete
        self.header_autocomplete_visible = false;
        self.header_autocomplete_selected = 0;
    }

    pub fn get_filtered_header_suggestions(&self) -> Vec<&'static str> {
        if !self.editing_header_key {
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

        // Sort by relevance (starts with query first, then by length)
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
        if max > 0 && self.header_autocomplete_selected < max - 1 {
            self.header_autocomplete_selected += 1;
        }
    }

    pub fn select_previous_autocomplete(&mut self) {
        if self.header_autocomplete_selected > 0 {
            self.header_autocomplete_selected -= 1;
        }
    }

    pub fn apply_autocomplete_selection(&mut self, suggestions: &[&str]) {
        if let Some(selected) = suggestions.get(self.header_autocomplete_selected) {
            self.header_key_buffer = selected.to_string();
            self.editing_header_key = false;
            self.header_autocomplete_visible = false;
            self.header_autocomplete_selected = 0;
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
    use crate::http_client::{HttpMethod, HttpRequest, HttpResponse};
    use std::collections::HashMap;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build an App with a pre-populated request list, bypassing disk I/O.
    fn app_with_requests(requests: Vec<HttpRequest>) -> App {
        let current_request_index = if requests.is_empty() { None } else { Some(0) };
        App {
            current_screen: CurrentScreen::Main,
            requests,
            current_request_index,
            pending_response: None,
            pending_request_index: None,
            event_receiver: None,
            request_events_per: HashMap::new(),
            last_response_per: HashMap::new(),
            http_runtime: HttpRuntime::new().expect("runtime"),
            editing_field: None,
            focused_field: FocusableField::Url,
            input_buffer: String::new(),
            header_key_buffer: String::new(),
            header_value_buffer: String::new(),
            editing_header_key: true,
            editing_existing_header: None,
            selected_header_index: 0,
            header_autocomplete_visible: false,
            header_autocomplete_selected: 0,
            response_view_mode: ResponseViewMode::Text,
            headers_scroll: 0,
            body_scroll: 0,
            events_scroll: 0,
            response_scroll: 0,
            streamed_body_per: HashMap::new(),
            last_save_status: None,
            keymap: Keymap::default(),
            streamed_jq_available: false,
            streamed_jq_output_per: HashMap::new(),
            streamed_line_buffer_per: HashMap::new(),
            streamed_stripped_lines_per: HashMap::new(),
            streamed_jq_output_tx: None,
            streamed_jq_output_rx: None,
            streamed_jq_last_fed_per: HashMap::new(),
            streamed_jq_stdin: None,
            streamed_jq_child: None,
        }
    }

    fn make_get(url: &str) -> HttpRequest {
        HttpRequest::new(HttpMethod::GET, url.to_string())
    }

    fn make_response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            status_text: "OK".to_string(),
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
        assert_eq!(app.requests[0].url, "https://");
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
        assert_eq!(app.requests[0].url, "https://second.com");
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
        assert_eq!(app.get_current_request().unwrap().url, "https://first.com");
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
        assert!(app.header_autocomplete_visible);
        assert_eq!(app.header_autocomplete_selected, 0);
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
        app.header_autocomplete_visible = true;
        app.header_autocomplete_selected = 4;

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
        assert!(!app.header_autocomplete_visible);
        assert_eq!(app.header_autocomplete_selected, 0);
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
        app.editing_header_key = true;
        app.header_key_buffer = String::new();
        let suggestions = app.get_filtered_header_suggestions();
        // There are 76 standard headers — just verify we get a non-empty full list
        assert!(!suggestions.is_empty());
        assert!(suggestions.len() > 50);
    }

    #[test]
    fn test_get_filtered_header_suggestions_filters_by_prefix() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.editing_header_key = true;
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
        app.editing_header_key = false;
        app.header_key_buffer = "content".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_get_filtered_header_suggestions_sorted_prefix_first() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.editing_header_key = true;
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
        app.editing_header_key = true;
        app.header_key_buffer = "zzzzzzzzzz".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_select_next_autocomplete_increments() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete_selected = 0;
        app.select_next_autocomplete(5);
        assert_eq!(app.header_autocomplete_selected, 1);
    }

    #[test]
    fn test_select_next_autocomplete_does_not_exceed_max() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete_selected = 4;
        app.select_next_autocomplete(5); // max index is 4
        assert_eq!(app.header_autocomplete_selected, 4);
    }

    #[test]
    fn test_select_previous_autocomplete_decrements() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete_selected = 3;
        app.select_previous_autocomplete();
        assert_eq!(app.header_autocomplete_selected, 2);
    }

    #[test]
    fn test_select_previous_autocomplete_does_not_underflow() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete_selected = 0;
        app.select_previous_autocomplete();
        assert_eq!(app.header_autocomplete_selected, 0);
    }

    #[test]
    fn test_apply_autocomplete_selection_sets_key_and_hides() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete_selected = 1;
        app.header_autocomplete_visible = true;
        let suggestions = &["content-type", "content-length", "authorization"];
        app.apply_autocomplete_selection(suggestions);
        assert_eq!(app.header_key_buffer, "content-length");
        assert!(!app.editing_header_key);
        assert!(!app.header_autocomplete_visible);
        assert_eq!(app.header_autocomplete_selected, 0);
    }

    #[test]
    fn test_apply_autocomplete_selection_out_of_bounds_is_noop() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.header_autocomplete_selected = 99;
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
        app.last_response_per
            .insert(0, Ok(make_response(200, r#"{"key":"value"}"#)));
        assert!(app.is_response_json());
    }

    #[test]
    fn test_is_response_json_false_for_plain_text() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.last_response_per
            .insert(0, Ok(make_response(200, "plain text response")));
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
        app.last_response_per
            .insert(0, Err("request failed".to_string()));
        assert!(!app.is_response_json());
    }

    #[test]
    fn test_cycle_response_view_mode_text_to_json_when_json_available() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.last_response_per
            .insert(0, Ok(make_response(200, r#"{"ok":true}"#)));
        app.response_view_mode = ResponseViewMode::Text;
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::Json);
    }

    #[test]
    fn test_cycle_response_view_mode_stays_text_when_not_json() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.last_response_per
            .insert(0, Ok(make_response(200, "not json")));
        app.response_view_mode = ResponseViewMode::Text;
        app.cycle_response_view_mode();
        assert_eq!(app.response_view_mode, ResponseViewMode::Text);
    }

    #[test]
    fn test_cycle_response_view_mode_json_to_text() {
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.last_response_per
            .insert(0, Ok(make_response(200, r#"{"ok":true}"#)));
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
            assert_eq!(resp.status, 200);
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
        assert_eq!(loaded[0].url, "https://save-test.com");
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
        assert_eq!(loaded[0].url, "https://second.com");
    }

    // ── fuzzy_match (module-private, tested via get_filtered_header_suggestions)

    #[test]
    fn test_fuzzy_match_via_suggestions_subsequence() {
        // "ct" should match "content-type" (c…t subsequence)
        let mut app = app_with_requests(vec![make_get("https://a.com")]);
        app.editing_header_key = true;
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
        app.editing_header_key = true;
        app.header_key_buffer = "xyz_not_a_header_xyz".to_string();
        let suggestions = app.get_filtered_header_suggestions();
        assert!(suggestions.is_empty());
    }
}
