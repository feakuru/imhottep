use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use tokio::sync::{mpsc, oneshot};

// Re-export hyper's Method as HttpMethod for compatibility
pub use hyper::Method as HttpMethod;

mod http_method_serde {
    use hyper::Method;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(method: &Method, s: S) -> Result<S::Ok, S::Error> {
        method.as_str().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Method, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<Method>().map_err(serde::de::Error::custom)
    }
}

/// Events emitted during HTTP request lifecycle
#[derive(Debug, Clone)]
pub enum RequestEvent {
    Started,
    ResolvingHost(String),
    HostResolved,
    Connecting,
    TlsHandshakeStarted,
    TlsHandshakeComplete,
    SendingRequest,
    RequestSent,
    WaitingForResponse,
    ReceivingHeaders,
    HeadersReceived(u16), // status code
    ReceivingBody(usize), // bytes received so far
    Completed,
    Failed(String),
}

impl fmt::Display for RequestEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestEvent::Started => write!(f, "Request started"),
            RequestEvent::ResolvingHost(host) => write!(f, "Resolving host: {}", host),
            RequestEvent::HostResolved => write!(f, "Host resolved"),
            RequestEvent::Connecting => write!(f, "Connecting to server"),
            RequestEvent::TlsHandshakeStarted => write!(f, "Starting TLS handshake"),
            RequestEvent::TlsHandshakeComplete => write!(f, "TLS handshake complete"),
            RequestEvent::SendingRequest => write!(f, "Sending request"),
            RequestEvent::RequestSent => write!(f, "Request sent"),
            RequestEvent::WaitingForResponse => write!(f, "Waiting for response"),
            RequestEvent::ReceivingHeaders => write!(f, "Receiving headers"),
            RequestEvent::HeadersReceived(status) => {
                write!(f, "Headers received (Status: {})", status)
            }
            RequestEvent::ReceivingBody(bytes) => write!(f, "Receiving body ({} bytes)", bytes),
            RequestEvent::Completed => write!(f, "Request completed"),
            RequestEvent::Failed(err) => write!(f, "Request failed: {}", err),
        }
    }
}

/// Represents an HTTP request to be sent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    #[serde(with = "http_method_serde")]
    pub method: HttpMethod,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

impl HttpRequest {
    pub fn new(method: HttpMethod, url: String) -> Self {
        Self {
            method,
            url,
            headers: HashMap::new(),
            body: None,
        }
    }

    pub fn with_header(mut self, key: String, value: String) -> Self {
        self.headers.insert(key, value);
        self
    }

    pub fn with_body(mut self, body: String) -> Self {
        self.body = Some(body);
        self
    }

    pub fn add_header(&mut self, key: String, value: String) {
        self.headers.insert(key, value);
    }

    pub fn set_body(&mut self, body: String) {
        self.body = Some(body);
    }

    pub fn remove_header(&mut self, key: &str) -> Option<String> {
        self.headers.remove(key)
    }
}

/// Represents an HTTP response received
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    pub fn is_client_error(&self) -> bool {
        self.status >= 400 && self.status < 500
    }

    pub fn is_server_error(&self) -> bool {
        self.status >= 500 && self.status < 600
    }
}

/// Error types for HTTP operations
#[derive(Debug)]
pub enum HttpError {
    InvalidUrl(String),
    InvalidHeader(String),
    RequestFailed(String),
    ResponseParseError(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::InvalidUrl(msg) => write!(f, "Invalid URL: {}", msg),
            HttpError::InvalidHeader(msg) => write!(f, "Invalid header: {}", msg),
            HttpError::RequestFailed(msg) => write!(f, "Request failed: {}", msg),
            HttpError::ResponseParseError(msg) => write!(f, "Response parse error: {}", msg),
        }
    }
}

impl Error for HttpError {}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a `RequestEvent` if a channel is present. Ignores send errors (receiver
/// may have been dropped).
fn send_event(tx: &Option<mpsc::UnboundedSender<RequestEvent>>, event: RequestEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Emit a `Failed` event and convert an error message into an `HttpError`.
fn fail<F>(tx: &Option<mpsc::UnboundedSender<RequestEvent>>, msg: String, make_err: F) -> HttpError
where
    F: FnOnce(String) -> HttpError,
{
    send_event(tx, RequestEvent::Failed(msg.clone()));
    make_err(msg)
}

// ---------------------------------------------------------------------------
// HttpClient
// ---------------------------------------------------------------------------

type HyperClient = Client<
    hyper_tls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    Full<Bytes>,
>;

/// HTTP client for executing requests
#[derive(Clone)]
pub struct HttpClient {
    client: HyperClient,
}

impl HttpClient {
    pub fn new() -> Self {
        let https = hyper_tls::HttpsConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(https);
        Self { client }
    }

    /// Execute an HTTP request asynchronously with event notifications.
    pub async fn execute(
        &self,
        request: HttpRequest,
        event_tx: Option<mpsc::UnboundedSender<RequestEvent>>,
    ) -> Result<HttpResponse, HttpError> {
        let tx = &event_tx;

        send_event(tx, RequestEvent::Started);

        // Parse the URL
        let uri = request
            .url
            .parse::<hyper::Uri>()
            .map_err(|e| fail(tx, e.to_string(), HttpError::InvalidUrl))?;

        // Emit host-resolution events
        if let Some(host) = uri.host() {
            send_event(tx, RequestEvent::ResolvingHost(host.to_string()));
        }
        send_event(tx, RequestEvent::HostResolved);
        send_event(tx, RequestEvent::Connecting);

        // TLS handshake events happen during the connection phase
        let is_https = uri.scheme_str() == Some("https");
        if is_https {
            send_event(tx, RequestEvent::TlsHandshakeStarted);
            send_event(tx, RequestEvent::TlsHandshakeComplete);
        }

        // Build the hyper request
        let mut req_builder = Request::builder().method(&request.method).uri(uri);

        for (key, value) in &request.headers {
            req_builder = req_builder.header(
                key.as_str(),
                value
                    .parse::<hyper::header::HeaderValue>()
                    .map_err(|e| fail(tx, format!("{}: {}", key, e), HttpError::InvalidHeader))?,
            );
        }

        let body_bytes = Bytes::from(request.body.unwrap_or_default());
        let hyper_request = req_builder
            .body(Full::new(body_bytes))
            .map_err(|e| fail(tx, e.to_string(), HttpError::RequestFailed))?;

        send_event(tx, RequestEvent::SendingRequest);

        // Execute the request
        let response = self
            .client
            .request(hyper_request)
            .await
            .map_err(|e| fail(tx, e.to_string(), HttpError::RequestFailed))?;

        send_event(tx, RequestEvent::RequestSent);
        send_event(tx, RequestEvent::WaitingForResponse);
        send_event(tx, RequestEvent::ReceivingHeaders);

        self.parse_response(response, event_tx).await
    }

    async fn parse_response(
        &self,
        response: Response<Incoming>,
        event_tx: Option<mpsc::UnboundedSender<RequestEvent>>,
    ) -> Result<HttpResponse, HttpError> {
        let tx = &event_tx;

        let status = response.status();
        let status_code = status.as_u16();
        let status_text = status.canonical_reason().unwrap_or("Unknown").to_string();

        send_event(tx, RequestEvent::HeadersReceived(status_code));

        // Extract headers
        let mut headers = HashMap::new();
        for (key, value) in response.headers() {
            if let Ok(value_str) = value.to_str() {
                headers.insert(key.to_string(), value_str.to_string());
            }
        }

        // Read body
        send_event(tx, RequestEvent::ReceivingBody(0));

        let body_bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| fail(tx, e.to_string(), HttpError::ResponseParseError))?
            .to_bytes();

        send_event(tx, RequestEvent::ReceivingBody(body_bytes.len()));

        let body = String::from_utf8_lossy(&body_bytes).into_owned();

        send_event(tx, RequestEvent::Completed);

        Ok(HttpResponse {
            status: status_code,
            status_text,
            headers,
            body,
        })
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// HttpRuntime
// ---------------------------------------------------------------------------

/// HTTP runtime manager that persists for the app lifetime
pub struct HttpRuntime {
    runtime: tokio::runtime::Runtime,
    client: HttpClient,
}

impl HttpRuntime {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let client = HttpClient::new();
        Ok(Self { runtime, client })
    }

    /// Execute an HTTP request in the background.
    /// Returns a receiver for the result and a receiver for events.
    pub fn execute_request(
        &self,
        request: HttpRequest,
    ) -> (
        oneshot::Receiver<Result<HttpResponse, HttpError>>,
        mpsc::UnboundedReceiver<RequestEvent>,
    ) {
        let (result_tx, result_rx) = oneshot::channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let client = self.client.clone();

        self.runtime.spawn(async move {
            let result = client.execute(request, Some(event_tx)).await;
            let _ = result_tx.send(result);
        });

        (result_rx, event_rx)
    }
}

impl Default for HttpRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create HTTP runtime")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_method_display() {
        assert_eq!(HttpMethod::GET.to_string(), "GET");
        assert_eq!(HttpMethod::POST.to_string(), "POST");
        assert_eq!(HttpMethod::PUT.to_string(), "PUT");
        assert_eq!(HttpMethod::DELETE.to_string(), "DELETE");
        assert_eq!(HttpMethod::PATCH.to_string(), "PATCH");
    }

    #[test]
    fn test_http_request_builder() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com".to_string())
            .with_header("Content-Type".to_string(), "application/json".to_string())
            .with_body("test body".to_string());

        assert_eq!(req.method, HttpMethod::GET);
        assert_eq!(req.url, "https://example.com");
        assert_eq!(
            req.headers.get("Content-Type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(req.body, Some("test body".to_string()));
    }

    #[test]
    fn test_http_request_add_header() {
        let mut req = HttpRequest::new(HttpMethod::POST, "https://example.com".to_string());
        req.add_header("Authorization".to_string(), "Bearer token".to_string());

        assert_eq!(
            req.headers.get("Authorization"),
            Some(&"Bearer token".to_string())
        );
    }

    #[test]
    fn test_http_request_remove_header() {
        let mut req = HttpRequest::new(HttpMethod::GET, "https://example.com".to_string())
            .with_header("X-Test".to_string(), "value".to_string());

        let removed = req.remove_header("X-Test");
        assert_eq!(removed, Some("value".to_string()));
        assert_eq!(req.headers.get("X-Test"), None);
    }

    #[test]
    fn test_http_request_set_body() {
        let mut req = HttpRequest::new(HttpMethod::POST, "https://example.com".to_string());
        req.set_body("initial body".to_string());
        assert_eq!(req.body, Some("initial body".to_string()));

        req.set_body("updated body".to_string());
        assert_eq!(req.body, Some("updated body".to_string()));
    }

    #[test]
    fn test_http_response_status_checks() {
        let success_response = HttpResponse {
            status: 200,
            status_text: "OK".to_string(),
            headers: HashMap::new(),
            body: "".to_string(),
        };
        assert!(success_response.is_success());
        assert!(!success_response.is_client_error());
        assert!(!success_response.is_server_error());

        let client_error_response = HttpResponse {
            status: 404,
            status_text: "Not Found".to_string(),
            headers: HashMap::new(),
            body: "".to_string(),
        };
        assert!(!client_error_response.is_success());
        assert!(client_error_response.is_client_error());
        assert!(!client_error_response.is_server_error());

        let server_error_response = HttpResponse {
            status: 500,
            status_text: "Internal Server Error".to_string(),
            headers: HashMap::new(),
            body: "".to_string(),
        };
        assert!(!server_error_response.is_success());
        assert!(!server_error_response.is_client_error());
        assert!(server_error_response.is_server_error());
    }

    #[test]
    fn test_invalid_url_error() {
        let error = HttpError::InvalidUrl("not a valid url".to_string());
        assert!(error.to_string().contains("Invalid URL"));
    }

    #[test]
    fn test_request_builder_chaining() {
        let req = HttpRequest::new(HttpMethod::POST, "https://api.example.com/data".to_string())
            .with_header("Content-Type".to_string(), "application/json".to_string())
            .with_header("Authorization".to_string(), "Bearer token123".to_string())
            .with_body(r#"{"key": "value"}"#.to_string());

        assert_eq!(req.headers.len(), 2);
        assert!(req.body.is_some());
    }

    #[test]
    fn test_http_request_empty_body() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com".to_string());
        assert_eq!(req.body, None);
    }

    #[test]
    fn test_http_request_empty_headers() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com".to_string());
        assert_eq!(req.headers.len(), 0);
    }
}
