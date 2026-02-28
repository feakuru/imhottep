use crate::http_client::{HttpRequest, HttpResponse, HttpRuntime, RequestEvent};
use hyper::Method;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseViewMode {
    Text,
    Json,
}

impl ResponseViewMode {
    pub fn label(&self) -> &'static str {
        match self {
            ResponseViewMode::Text => "text",
            ResponseViewMode::Json => "json",
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
    pub event_receiver: Option<mpsc::UnboundedReceiver<RequestEvent>>,
    pub request_events: Vec<String>,
    pub last_response: Option<Result<HttpResponse, String>>,
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
    // jq filter expression for JSON view mode
    pub jq_filter: String,
    // Scroll positions for each field
    pub headers_scroll: u16,
    pub body_scroll: u16,
    pub events_scroll: u16,
    pub response_scroll: u16,
    // Last save status message (shown briefly in the footer)
    pub last_save_status: Option<String>,
}

impl App {
    pub fn new() -> App {
        let requests = load_requests().unwrap_or_default();
        let current_request_index = if requests.is_empty() { None } else { Some(0) };
        App {
            current_screen: CurrentScreen::Main,
            requests,
            current_request_index,
            pending_response: None,
            event_receiver: None,
            request_events: Vec::new(),
            last_response: None,
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
            jq_filter: ".".to_string(),
            headers_scroll: 0,
            body_scroll: 0,
            events_scroll: 0,
            response_scroll: 0,
            last_save_status: None,
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

    pub fn create_new_request(&mut self) {
        let new_request = HttpRequest::new(Method::GET, "https://".to_string());
        self.requests.push(new_request);
        self.current_request_index = Some(self.requests.len() - 1);
    }

    pub fn delete_current_request(&mut self) {
        if let Some(idx) = self.current_request_index {
            if idx < self.requests.len() {
                self.requests.remove(idx);
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
        if let Some(request) = self.get_current_request().cloned() {
            let (result_rx, event_rx) = self.http_runtime.execute_request(request);
            self.pending_response = Some(result_rx);
            self.event_receiver = Some(event_rx);
            self.request_events.clear();
            self.last_response = None;
        }
    }

    pub fn check_pending_response(&mut self) {
        if let Some(receiver) = &mut self.pending_response {
            match receiver.try_recv() {
                Ok(result) => {
                    self.last_response = Some(result.map_err(|e| e.to_string()));
                    self.pending_response = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still waiting
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.last_response = Some(Err("Request channel closed".to_string()));
                    self.pending_response = None;
                }
            }
        }
    }

    pub fn check_for_events(&mut self) -> bool {
        let mut received_any = false;
        if let Some(receiver) = &mut self.event_receiver {
            // Drain all available events
            while let Ok(event) = receiver.try_recv() {
                self.request_events.push(event.to_string());
                received_any = true;
            }
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

    /// Returns true when the last response body is valid JSON (parsed by serde_json).
    pub fn is_response_json(&self) -> bool {
        if let Some(Ok(ref resp)) = self.last_response {
            serde_json::from_str::<serde_json::Value>(&resp.body).is_ok()
        } else {
            false
        }
    }

    /// Cycles to the next available view mode.  Json mode is only entered when
    /// the response body is valid JSON; otherwise it is skipped.
    pub fn cycle_response_view_mode(&mut self) {
        self.response_view_mode = match self.response_view_mode {
            ResponseViewMode::Text => {
                if self.is_response_json() {
                    ResponseViewMode::Json
                } else {
                    ResponseViewMode::Text
                }
            }
            ResponseViewMode::Json => ResponseViewMode::Text,
        };
    }

    /// Runs the current `jq_filter` against the last response body via the
    /// `jq` binary and returns the colourised output, or an error string.
    pub fn run_jq(&self) -> String {
        let body = match &self.last_response {
            Some(Ok(resp)) => &resp.body,
            _ => return String::new(),
        };
        let filter = if self.jq_filter.trim().is_empty() {
            "."
        } else {
            self.jq_filter.as_str()
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
            FocusableField::Body => FocusableField::RequestEvents,
            FocusableField::RequestEvents => FocusableField::Response,
            FocusableField::Response => FocusableField::Url,
        };
    }

    pub fn focus_previous_field(&mut self) {
        self.focused_field = match self.focused_field {
            FocusableField::Url => FocusableField::Response,
            FocusableField::Headers => FocusableField::Url,
            FocusableField::Body => FocusableField::Headers,
            FocusableField::RequestEvents => FocusableField::Body,
            FocusableField::Response => FocusableField::RequestEvents,
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

    pub fn scroll_up(&mut self) {
        if let Some(scroll) = self.focused_scroll() {
            *scroll = scroll.saturating_sub(1);
        }
    }

    pub fn scroll_down(&mut self) {
        if let Some(scroll) = self.focused_scroll() {
            *scroll = scroll.saturating_add(1);
        }
    }

    #[allow(dead_code)]
    pub fn clamp_scroll(&mut self, max_scroll: u16) {
        if let Some(scroll) = self.focused_scroll() {
            *scroll = (*scroll).min(max_scroll);
        }
    }

    #[allow(dead_code)]
    pub fn reset_scroll_for_field(&mut self, field: FocusableField) {
        match field {
            FocusableField::Headers => self.headers_scroll = 0,
            FocusableField::Body => self.body_scroll = 0,
            FocusableField::RequestEvents => self.events_scroll = 0,
            FocusableField::Response => self.response_scroll = 0,
            FocusableField::Url => {}
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
        match save_requests(&self.requests) {
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

fn library_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p
            })
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("imhottep").join("request-library.json")
}

pub fn load_requests() -> Result<Vec<HttpRequest>, Box<dyn std::error::Error>> {
    let path = library_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)?;
    let requests = serde_json::from_str(&data)?;
    Ok(requests)
}

pub fn save_requests(requests: &[HttpRequest]) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = library_path();
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
