use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::ops::Deref;
use tokio::sync::{mpsc, oneshot};

pub use hyper::Method as HttpMethod;

// ── Newtypes ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UrlString(String);

impl UrlString {
    pub fn new(s: String) -> Self {
        Self(s)
    }
}

impl Deref for UrlString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UrlString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<UrlString> for String {
    fn from(u: UrlString) -> String {
        u.0
    }
}

impl From<String> for UrlString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl<'a> From<&'a str> for UrlString {
    fn from(s: &'a str) -> Self {
        Self(s.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HeaderName(String);

impl HeaderName {
    pub fn new(s: String) -> Self {
        Self(s)
    }
}

impl Deref for HeaderName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HeaderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Borrow<str> for HeaderName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for HeaderName {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl<'a> From<&'a str> for HeaderName {
    fn from(s: &'a str) -> Self {
        Self(s.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HeaderValue(String);

impl HeaderValue {
    pub fn new(s: String) -> Self {
        Self(s)
    }
}

impl Deref for HeaderValue {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HeaderValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Borrow<str> for HeaderValue {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for HeaderValue {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl<'a> From<&'a str> for HeaderValue {
    fn from(s: &'a str) -> Self {
        Self(s.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JqFilter(String);

impl JqFilter {
    pub fn new(s: String) -> Self {
        Self(s)
    }
}

impl Deref for JqFilter {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JqFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Default for JqFilter {
    fn default() -> Self {
        Self(".".to_string())
    }
}

impl From<String> for JqFilter {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl<'a> From<&'a str> for JqFilter {
    fn from(s: &'a str) -> Self {
        Self(s.to_owned())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegexPattern {
    pattern: String,
    #[serde(skip)]
    compiled: Option<Box<Regex>>,
}

impl RegexPattern {
    pub fn new(pattern: String) -> Self {
        let compiled = Regex::new(&pattern).ok().map(Box::new);
        Self { pattern, compiled }
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn compiled(&self) -> Option<&Regex> {
        self.compiled.as_deref()
    }

    pub fn compile(&self) -> Result<Regex, regex::Error> {
        Regex::new(&self.pattern)
    }

    pub fn set_pattern(&mut self, pattern: String) {
        self.pattern = pattern;
        self.compiled = Regex::new(&self.pattern).ok().map(Box::new);
    }
}

impl Deref for RegexPattern {
    type Target = str;
    fn deref(&self) -> &str {
        &self.pattern
    }
}

impl fmt::Display for RegexPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pattern.fmt(f)
    }
}

impl PartialEq for RegexPattern {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}

impl Eq for RegexPattern {}

impl From<String> for RegexPattern {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl<'a> From<&'a str> for RegexPattern {
    fn from(s: &'a str) -> Self {
        Self::new(s.to_owned())
    }
}

fn default_jq_filter() -> JqFilter {
    JqFilter::default()
}

fn is_default_jq_filter(f: &JqFilter) -> bool {
    f.deref() == "."
}


fn default_prefix_regex() -> RegexPattern {
    RegexPattern::new(r"^\w+:\s*".to_string())
}

fn is_default_prefix_regex(r: &RegexPattern) -> bool {
    r.pattern() == r"^\w+:\s*"
}

fn default_suffix_regex() -> RegexPattern {
    RegexPattern::new(r"\s*$".to_string())
}

fn is_default_suffix_regex(r: &RegexPattern) -> bool {
    r.pattern() == r"\s*$"
}

// ── Events ────────────────────────────────────────────────────────────────────

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
    HeadersReceived(u16),
    BodyChunk(String),
    Completed(usize),
    TemporaryConnectionProblem(String),
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
            RequestEvent::BodyChunk(s) => write!(f, "Body chunk ({} bytes)", s.len()),
            RequestEvent::Completed(sz) => write!(f, "Request completed: {} total bytes", sz),
            RequestEvent::TemporaryConnectionProblem(s) => {
                write!(f, "Temporary issue with connection: {}", s)
            }
            RequestEvent::Failed(err) => write!(f, "Request failed: {}", err),
        }
    }
}

// ── HTTP request ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    #[serde(with = "http_method_serde")]
    pub method: HttpMethod,
    pub url: UrlString,
    #[serde(with = "header_map_serde")]
    pub headers: HashMap<HeaderName, HeaderValue>,
    pub body: Option<String>,
    #[serde(default = "default_jq_filter", skip_serializing_if = "is_default_jq_filter")]
    pub jq_filter: JqFilter,
    #[serde(
        default = "default_prefix_regex",
        skip_serializing_if = "is_default_prefix_regex",
        with = "regex_pattern_serde"
    )]
    pub stream_prefix_regex: RegexPattern,
    #[serde(
        default = "default_suffix_regex",
        skip_serializing_if = "is_default_suffix_regex",
        with = "regex_pattern_serde"
    )]
    pub stream_suffix_regex: RegexPattern,
}

impl HttpRequest {
    pub fn new(method: HttpMethod, url: impl Into<UrlString>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HashMap::new(),
            body: None,
            jq_filter: JqFilter::default(),
            stream_prefix_regex: default_prefix_regex(),
            stream_suffix_regex: default_suffix_regex(),
        }
    }

    pub fn with_header(mut self, key: impl Into<HeaderName>, value: impl Into<HeaderValue>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    pub fn with_body(mut self, body: String) -> Self {
        self.body = Some(body);
        self
    }

    pub fn add_header(&mut self, key: impl Into<HeaderName>, value: impl Into<HeaderValue>) {
        self.headers.insert(key.into(), value.into());
    }

    pub fn set_body(&mut self, body: String) {
        self.body = Some(body);
    }

    pub fn remove_header(&mut self, key: &str) -> Option<HeaderValue> {
        let hk = key.to_lowercase();
        let target: Vec<HeaderName> = self
            .headers
            .keys()
            .filter(|k| k.to_lowercase() == hk)
            .cloned()
            .collect();
        target.into_iter().next().and_then(|k| self.headers.remove(&k))
    }
}

// ── HTTP response ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status_code: StatusCode,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        self.status_code.is_success()
    }

    pub fn is_client_error(&self) -> bool {
        self.status_code.is_client_error()
    }

    pub fn is_server_error(&self) -> bool {
        self.status_code.is_server_error()
    }
}

// ── HttpError with source chaining ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum HttpError {
    InvalidUrl { msg: String, source: Option<Box<ErrorSource>> },
    InvalidHeader { msg: String, source: Option<Box<ErrorSource>> },
    RequestFailed { msg: String, source: Option<Box<ErrorSource>> },
    ResponseParseError { msg: String, source: Option<Box<ErrorSource>> },
}

// Wrapper type so we can derive Clone on HttpError
#[derive(Debug)]
pub struct ErrorSource(Box<dyn Error + Send + Sync>);

impl Clone for ErrorSource {
    fn clone(&self) -> Self {
        // For display/error purposes, we store the error message.
        // This is lossy but enables Clone.
        Self(Box::new(std::io::Error::new(std::io::ErrorKind::Other, self.0.to_string())))
    }
}

impl Error for ErrorSource {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

impl fmt::Display for ErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::InvalidUrl { msg, .. } => write!(f, "Invalid URL: {}", msg),
            HttpError::InvalidHeader { msg, .. } => write!(f, "Invalid header: {}", msg),
            HttpError::RequestFailed { msg, .. } => write!(f, "Request failed: {}", msg),
            HttpError::ResponseParseError { msg, .. } => write!(f, "Response parse error: {}", msg),
        }
    }
}

impl Error for HttpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            HttpError::InvalidUrl { source, .. }
            | HttpError::InvalidHeader { source, .. }
            | HttpError::RequestFailed { source, .. }
            | HttpError::ResponseParseError { source, .. } => {
                source.as_ref().map(|s| s as &(dyn Error + 'static))
            }
        }
    }
}

// ── Serde helpers ─────────────────────────────────────────────────────────────

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

mod header_map_serde {
    use crate::http_client::{HeaderName, HeaderValue};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S: Serializer>(
        map: &HashMap<HeaderName, HeaderValue>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let raw: HashMap<&str, &str> = map
            .iter()
            .map(|(k, v)| (k.as_ref(), v.as_ref()))
            .collect();
        raw.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<HashMap<HeaderName, HeaderValue>, D::Error> {
        let raw: HashMap<String, String> = HashMap::deserialize(d)?;
        Ok(raw
            .into_iter()
            .map(|(k, v)| (HeaderName::new(k), HeaderValue::new(v)))
            .collect())
    }
}

mod regex_pattern_serde {
    use crate::http_client::RegexPattern;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(r: &RegexPattern, s: S) -> Result<S::Ok, S::Error> {
        r.pattern().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RegexPattern, D::Error> {
        let pattern = String::deserialize(d)?;
        Ok(RegexPattern::new(pattern))
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn send_event(tx: &Option<mpsc::UnboundedSender<RequestEvent>>, event: RequestEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

fn fail(
    tx: &Option<mpsc::UnboundedSender<RequestEvent>>,
    msg: String,
    source: Option<Box<dyn Error + Send + Sync>>,
    variant: fn(String, Option<Box<ErrorSource>>) -> HttpError,
) -> HttpError {
    send_event(tx, RequestEvent::Failed(msg.clone()));
    variant(msg, source.map(|s| Box::new(ErrorSource(s))))
}

// ── HttpClient ────────────────────────────────────────────────────────────────

type HyperClient = Client<
    hyper_tls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    Full<Bytes>,
>;

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

    pub async fn execute(
        &self,
        request: HttpRequest,
        event_tx: Option<mpsc::UnboundedSender<RequestEvent>>,
    ) -> Result<HttpResponse, HttpError> {
        let tx = &event_tx;

        send_event(tx, RequestEvent::Started);

        let uri = url::Url::parse(&request.url)
            .map_err(|e| {
                fail(
                    tx,
                    e.to_string(),
                    Some(Box::new(e)),
                    |msg, source| HttpError::InvalidUrl { msg, source },
                )
            })?
            .as_str()
            .parse::<hyper::Uri>()
            .map_err(|e| {
                fail(
                    tx,
                    e.to_string(),
                    Some(Box::new(e)),
                    |msg, source| HttpError::InvalidUrl { msg, source },
                )
            })?;

        if let Some(host) = uri.host() {
            send_event(tx, RequestEvent::ResolvingHost(host.to_string()));
        }
        send_event(tx, RequestEvent::HostResolved);
        send_event(tx, RequestEvent::Connecting);

        let is_https = uri.scheme_str() == Some("https");
        if is_https {
            send_event(tx, RequestEvent::TlsHandshakeStarted);
            send_event(tx, RequestEvent::TlsHandshakeComplete);
        }

        let mut req_builder = Request::builder().method(&request.method).uri(uri);

        for (key, value) in &request.headers {
            let hv: hyper::header::HeaderValue = value
                .parse()
                .map_err(|e| {
                    fail(
                        tx,
                        format!("{}: {}", key, e),
                        None,
                        |msg, source| HttpError::InvalidHeader { msg, source },
                    )
                })?;
            req_builder = req_builder.header(key.as_ref() as &str, hv);
        }

        let body_bytes = Bytes::from(request.body.unwrap_or_default());
        let hyper_request = req_builder
            .body(Full::new(body_bytes))
            .map_err(|e| {
                fail(
                    tx,
                    e.to_string(),
                    Some(Box::new(e)),
                    |msg, source| HttpError::RequestFailed { msg, source },
                )
            })?;

        send_event(tx, RequestEvent::SendingRequest);

        let response = self
            .client
            .request(hyper_request)
            .await
            .map_err(|e| {
                fail(
                    tx,
                    e.to_string(),
                    Some(Box::new(e)),
                    |msg, source| HttpError::RequestFailed { msg, source },
                )
            })?;

        send_event(tx, RequestEvent::RequestSent);
        send_event(tx, RequestEvent::WaitingForResponse);
        send_event(tx, RequestEvent::ReceivingHeaders);

        self.parse_response(response, event_tx).await
    }

    async fn parse_response(
        &self,
        mut response: Response<Incoming>,
        event_tx: Option<mpsc::UnboundedSender<RequestEvent>>,
    ) -> Result<HttpResponse, HttpError> {
        let tx = &event_tx;

        let status = response.status();
        let status_code = status.as_u16();

        send_event(tx, RequestEvent::HeadersReceived(status_code));

        let mut headers: HashMap<String, String> = HashMap::new();
        for (key, value) in response.headers() {
            if let Ok(value_str) = value.to_str() {
                headers.insert(key.to_string(), value_str.to_string());
            }
        }

        let mut accumulated_body = String::new();

        while let Some(next) = response.frame().await {
            match next {
                Ok(frame) => {
                    if let Some(chunk) = frame.data_ref() {
                        let chunk_str = String::from_utf8_lossy(chunk).into_owned();
                        accumulated_body = accumulated_body + &chunk_str;
                        send_event(tx, RequestEvent::BodyChunk(chunk_str));
                    }
                }
                Err(_) => {
                    send_event(
                        tx,
                        RequestEvent::TemporaryConnectionProblem(
                            "Could not receive response chunk".into(),
                        ),
                    );
                }
            }
        }
        send_event(tx, RequestEvent::Completed(accumulated_body.bytes().len()));

        Ok(HttpResponse {
            status_code: status,
            headers,
            body: accumulated_body,
        })
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

// ── HttpRuntime ───────────────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

    // ── HttpMethod / display ──────────────────────────────────────────────────

    #[test]
    fn test_http_method_display() {
        assert_eq!(HttpMethod::GET.to_string(), "GET");
        assert_eq!(HttpMethod::POST.to_string(), "POST");
        assert_eq!(HttpMethod::PUT.to_string(), "PUT");
        assert_eq!(HttpMethod::DELETE.to_string(), "DELETE");
        assert_eq!(HttpMethod::PATCH.to_string(), "PATCH");
    }

    #[test]
    fn test_http_method_head_options() {
        assert_eq!(HttpMethod::HEAD.to_string(), "HEAD");
        assert_eq!(HttpMethod::OPTIONS.to_string(), "OPTIONS");
    }

    // ── Newtype tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_url_string_from_string() {
        let u: UrlString = "https://example.com".into();
        assert_eq!(&*u, "https://example.com");
        assert_eq!(u.to_string(), "https://example.com");
    }

    #[test]
    fn test_url_string_serde_roundtrip() {
        let u = UrlString::new("https://example.com".to_string());
        let json = serde_json::to_string(&u).unwrap();
        assert_eq!(json, r#""https://example.com""#);
        let back: UrlString = serde_json::from_str(&json).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn test_header_name_serde_roundtrip() {
        let h: HeaderName = "Content-Type".into();
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json, r#""Content-Type""#);
        let back: HeaderName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn test_header_value_serde_roundtrip() {
        let v: HeaderValue = "application/json".into();
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#""application/json""#);
        let back: HeaderValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn test_jq_filter_default() {
        let f = JqFilter::default();
        assert_eq!(&*f, ".");
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, r#"".""#);
    }

    #[test]
    fn test_regex_pattern_compiles() {
        let r = RegexPattern::new(r"^\w+:\s*".to_string());
        assert!(r.compiled().is_some());
        assert!(r.compile().is_ok());
    }

    #[test]
    fn test_regex_pattern_invalid() {
        let r = RegexPattern::new(r"[invalid".to_string());
        assert!(r.compiled().is_none());
        assert!(r.compile().is_err());
    }

    #[test]
    fn test_regex_pattern_set_updates_compiled() {
        let mut r = RegexPattern::new(r"[invalid".to_string());
        assert!(r.compiled().is_none());
        r.set_pattern(r"^\w+:\s*".to_string());
        assert!(r.compiled().is_some());
    }

    #[test]
    fn test_regex_pattern_serde_roundtrip() {
        let r = RegexPattern::new(r"^\w+:\s*".to_string());
        let json = serde_json::to_string(&r).unwrap();
        // Direct struct serialization includes "pattern" field. The compiled
        // field is skipped, so it will be None after deserialization.
        let back: RegexPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pattern(), r.pattern());
    }

    #[test]
    fn test_regex_pattern_custom_serde_on_request() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com");
        let json = serde_json::to_string(&req).unwrap();
        // The custom regex_pattern_serde module serializes as a plain string
        // when used via `#[serde(with = "...")]` on HttpRequest fields.
        let decoded: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            req.stream_prefix_regex.pattern(),
            decoded.stream_prefix_regex.pattern()
        );
        assert_eq!(
            req.stream_suffix_regex.pattern(),
            decoded.stream_suffix_regex.pattern()
        );
        // Custom deserialization calls RegexPattern::new, which compiles.
        assert!(decoded.stream_prefix_regex.compiled().is_some());
        assert!(decoded.stream_suffix_regex.compiled().is_some());
    }

    // ── HttpRequest builder ───────────────────────────────────────────────────

    #[test]
    fn test_http_request_builder() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com")
            .with_header("Content-Type", "application/json")
            .with_body("test body".to_string());

        assert_eq!(req.method, HttpMethod::GET);
        assert_eq!(&*req.url, "https://example.com");
        assert_eq!(
            req.headers.get("Content-Type" as &str).map(|v| v.as_ref()),
            Some("application/json")
        );
        assert_eq!(req.body, Some("test body".to_string()));
    }

    #[test]
    fn test_http_request_add_header() {
        let mut req = HttpRequest::new(HttpMethod::POST, "https://example.com");
        req.add_header("Authorization", "Bearer token");

        assert_eq!(
            req.headers.get("Authorization" as &str).map(|v| v.as_ref()),
            Some("Bearer token")
        );
    }

    #[test]
    fn test_http_request_add_header_overwrites_existing() {
        let mut req = HttpRequest::new(HttpMethod::GET, "https://example.com");
        req.add_header("X-Custom", "first");
        req.add_header("X-Custom", "second");
        assert_eq!(
            req.headers.get("X-Custom" as &str).map(|v| v.as_ref()),
            Some("second")
        );
        assert_eq!(req.headers.len(), 1);
    }

    #[test]
    fn test_http_request_remove_header() {
        let mut req = HttpRequest::new(HttpMethod::GET, "https://example.com")
            .with_header("X-Test", "value");

        let removed = req.remove_header("X-Test");
        assert!(removed.is_some());
        assert_eq!(
            req.headers.get("X-Test" as &str),
            None
        );
    }

    #[test]
    fn test_http_request_remove_nonexistent_header_returns_none() {
        let mut req = HttpRequest::new(HttpMethod::GET, "https://example.com");
        let removed = req.remove_header("Does-Not-Exist");
        assert_eq!(removed, None);
    }

    #[test]
    fn test_http_request_set_body() {
        let mut req = HttpRequest::new(HttpMethod::POST, "https://example.com");
        req.set_body("initial body".to_string());
        assert_eq!(req.body, Some("initial body".to_string()));

        req.set_body("updated body".to_string());
        assert_eq!(req.body, Some("updated body".to_string()));
    }

    #[test]
    fn test_http_request_empty_body() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com");
        assert_eq!(req.body, None);
    }

    #[test]
    fn test_http_request_empty_headers() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com");
        assert_eq!(req.headers.len(), 0);
    }

    #[test]
    fn test_request_builder_chaining() {
        let req = HttpRequest::new(HttpMethod::POST, "https://api.example.com/data")
            .with_header("Content-Type", "application/json")
            .with_header("Authorization", "Bearer token123")
            .with_body(r#"{"key": "value"}"#.to_string());

        assert_eq!(req.headers.len(), 2);
        assert!(req.body.is_some());
    }

    // ── HttpRequest serialization round-trip ──────────────────────────────────

    #[test]
    fn test_http_request_serde_round_trip_get() {
        let req = HttpRequest::new(HttpMethod::GET, "https://example.com/api")
            .with_header("Accept", "application/json");

        let json = serde_json::to_string(&req).expect("serialization failed");
        let decoded: HttpRequest = serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(decoded.method, HttpMethod::GET);
        assert_eq!(&*decoded.url, "https://example.com/api");
        assert_eq!(
            decoded.headers.get("Accept" as &str).map(|v| v.as_ref()),
            Some("application/json")
        );
        assert_eq!(decoded.body, None);
    }

    #[test]
    fn test_http_request_serde_round_trip_post_with_body() {
        let req = HttpRequest::new(HttpMethod::POST, "https://api.example.com")
            .with_body(r#"{"name":"Alice"}"#.to_string());

        let json = serde_json::to_string(&req).expect("serialization failed");
        let decoded: HttpRequest = serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(decoded.method, HttpMethod::POST);
        assert_eq!(decoded.body, Some(r#"{"name":"Alice"}"#.to_string()));
    }

    #[test]
    fn test_http_request_serde_all_methods() {
        for method in [
            HttpMethod::GET,
            HttpMethod::POST,
            HttpMethod::PUT,
            HttpMethod::PATCH,
            HttpMethod::DELETE,
            HttpMethod::HEAD,
            HttpMethod::OPTIONS,
        ] {
            let req = HttpRequest::new(method.clone(), "https://example.com");
            let json = serde_json::to_string(&req).expect("serialization failed");
            let decoded: HttpRequest =
                serde_json::from_str(&json).expect("deserialization failed");
            assert_eq!(decoded.method, method);
        }
    }

    #[test]
    fn test_http_request_serde_method_stored_as_string() {
        let req = HttpRequest::new(HttpMethod::DELETE, "https://example.com");
        let json = serde_json::to_string(&req).expect("serialization failed");
        assert!(
            json.contains("\"DELETE\""),
            "expected plain string method in JSON: {json}"
        );
    }

    #[test]
    fn test_http_request_serde_vec_round_trip() {
        let requests = vec![
            HttpRequest::new(HttpMethod::GET, "https://a.example.com"),
            HttpRequest::new(HttpMethod::POST, "https://b.example.com")
                .with_body("data".to_string()),
        ];
        let json =
            serde_json::to_string_pretty(&requests).expect("serialization failed");
        let decoded: Vec<HttpRequest> =
            serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].method, HttpMethod::GET);
        assert_eq!(decoded[1].method, HttpMethod::POST);
        assert_eq!(decoded[1].body, Some("data".to_string()));
    }

    // ── HttpResponse status checks ────────────────────────────────────────────

    #[test]
    fn test_http_response_status_checks() {
        let success_response = HttpResponse {
            status_code: StatusCode::OK,
            headers: HashMap::new(),
            body: "".to_string(),
        };
        assert!(success_response.is_success());
        assert!(!success_response.is_client_error());
        assert!(!success_response.is_server_error());

        let client_error_response = HttpResponse {
            status_code: StatusCode::NOT_FOUND,
            headers: HashMap::new(),
            body: "".to_string(),
        };
        assert!(!client_error_response.is_success());
        assert!(client_error_response.is_client_error());
        assert!(!client_error_response.is_server_error());

        let server_error_response = HttpResponse {
            status_code: StatusCode::INTERNAL_SERVER_ERROR,
            headers: HashMap::new(),
            body: "".to_string(),
        };
        assert!(!server_error_response.is_success());
        assert!(!server_error_response.is_client_error());
        assert!(server_error_response.is_server_error());
    }

    #[test]
    fn test_http_response_status_boundary_values() {
        let r199 = HttpResponse {
            status_code: StatusCode::from_u16(199).unwrap(),
            headers: HashMap::new(),
            body: String::new(),
        };
        assert!(!r199.is_success());

        let r200 = HttpResponse {
            status_code: StatusCode::from_u16(200).unwrap(),
            ..r199.clone()
        };
        assert!(r200.is_success());

        let r299 = HttpResponse {
            status_code: StatusCode::from_u16(299).unwrap(),
            ..r199.clone()
        };
        assert!(r299.is_success());

        let r300 = HttpResponse {
            status_code: StatusCode::from_u16(300).unwrap(),
            ..r199.clone()
        };
        assert!(!r300.is_success());

        let r399 = HttpResponse {
            status_code: StatusCode::from_u16(399).unwrap(),
            ..r199.clone()
        };
        assert!(!r399.is_client_error());

        let r400 = HttpResponse {
            status_code: StatusCode::from_u16(400).unwrap(),
            ..r199.clone()
        };
        assert!(r400.is_client_error());

        let r499 = HttpResponse {
            status_code: StatusCode::from_u16(499).unwrap(),
            ..r199.clone()
        };
        assert!(r499.is_client_error());

        let r500 = HttpResponse {
            status_code: StatusCode::from_u16(500).unwrap(),
            ..r199.clone()
        };
        assert!(!r500.is_client_error());
        assert!(r500.is_server_error());

        let r599 = HttpResponse {
            status_code: StatusCode::from_u16(599).unwrap(),
            ..r199.clone()
        };
        assert!(r599.is_server_error());

        let r600 = HttpResponse {
            status_code: StatusCode::from_u16(600).unwrap(),
            ..r199
        };
        assert!(!r600.is_server_error());
    }

    #[test]
    fn test_http_response_all_status_categories_are_mutually_exclusive_for_common_codes() {
        let codes_and_expected: &[(u16, bool, bool, bool)] = &[
            (200, true, false, false),
            (201, true, false, false),
            (204, true, false, false),
            (301, false, false, false),
            (400, false, true, false),
            (401, false, true, false),
            (403, false, true, false),
            (404, false, true, false),
            (422, false, true, false),
            (500, false, false, true),
            (502, false, false, true),
            (503, false, false, true),
        ];
        for &(status, ok, ce, se) in codes_and_expected {
            let r = HttpResponse {
                status_code: StatusCode::from_u16(status).unwrap(),
                headers: HashMap::new(),
                body: String::new(),
            };
            assert_eq!(r.is_success(), ok, "is_success wrong for {status}");
            assert_eq!(
                r.is_client_error(),
                ce,
                "is_client_error wrong for {status}"
            );
            assert_eq!(
                r.is_server_error(),
                se,
                "is_server_error wrong for {status}"
            );
        }
    }

    // ── HttpError ─────────────────────────────────────────────────────────────

    #[test]
    fn test_invalid_url_error() {
        let error = HttpError::InvalidUrl {
            msg: "not a valid url".to_string(),
            source: None,
        };
        assert!(error.to_string().contains("Invalid URL"));
    }

    #[test]
    fn test_http_error_display_all_variants() {
        assert!(
            (HttpError::InvalidUrl {
                msg: "x".to_string(),
                source: None,
            })
            .to_string()
            .contains("Invalid URL")
        );
        assert!(
            (HttpError::InvalidHeader {
                msg: "y".to_string(),
                source: None,
            })
            .to_string()
            .contains("Invalid header")
        );
        assert!(
            (HttpError::RequestFailed {
                msg: "z".to_string(),
                source: None,
            })
            .to_string()
            .contains("Request failed")
        );
        assert!(
            (HttpError::ResponseParseError {
                msg: "w".to_string(),
                source: None,
            })
            .to_string()
            .contains("Response parse error")
        );
    }

    #[test]
    fn test_http_error_display_includes_message() {
        let msg = "something went wrong";
        let err = HttpError::RequestFailed {
            msg: msg.to_string(),
            source: None,
        };
        assert!(err.to_string().contains(msg));
    }

    #[test]
    fn test_http_error_implements_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(HttpError::InvalidUrl {
            msg: "bad".to_string(),
            source: None,
        });
        assert!(err.to_string().contains("Invalid URL"));
    }

    #[test]
    fn test_http_error_source_none() {
        let err = HttpError::InvalidUrl {
            msg: "bad".to_string(),
            source: None,
        };
        assert!(err.source().is_none());
    }

    #[test]
    fn test_http_error_source_some() {
        let inner = "inner error".to_string();
        let err = HttpError::InvalidUrl {
            msg: "bad".to_string(),
            source: Some(Box::new(ErrorSource(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidInput, inner))))),
        };
        assert!(err.source().is_some());
    }

    // ── RequestEvent display ──────────────────────────────────────────────────

    #[test]
    fn test_request_event_display_started() {
        assert_eq!(RequestEvent::Started.to_string(), "Request started");
    }

    #[test]
    fn test_request_event_display_resolving_host() {
        let ev = RequestEvent::ResolvingHost("example.com".to_string());
        assert!(ev.to_string().contains("Resolving host"));
        assert!(ev.to_string().contains("example.com"));
    }

    #[test]
    fn test_request_event_display_host_resolved() {
        assert_eq!(RequestEvent::HostResolved.to_string(), "Host resolved");
    }

    #[test]
    fn test_request_event_display_connecting() {
        assert_eq!(RequestEvent::Connecting.to_string(), "Connecting to server");
    }

    #[test]
    fn test_request_event_display_tls_events() {
        assert_eq!(
            RequestEvent::TlsHandshakeStarted.to_string(),
            "Starting TLS handshake"
        );
        assert_eq!(
            RequestEvent::TlsHandshakeComplete.to_string(),
            "TLS handshake complete"
        );
    }

    #[test]
    fn test_request_event_display_sending_and_waiting() {
        assert_eq!(RequestEvent::SendingRequest.to_string(), "Sending request");
        assert_eq!(RequestEvent::RequestSent.to_string(), "Request sent");
        assert_eq!(
            RequestEvent::WaitingForResponse.to_string(),
            "Waiting for response"
        );
        assert_eq!(
            RequestEvent::ReceivingHeaders.to_string(),
            "Receiving headers"
        );
    }

    #[test]
    fn test_request_event_display_headers_received() {
        let ev = RequestEvent::HeadersReceived(200);
        assert!(ev.to_string().contains("200"));
        assert!(ev.to_string().contains("Headers received"));
    }

    #[test]
    fn test_request_event_display_completed() {
        assert_eq!(
            RequestEvent::Completed(25).to_string(),
            "Request completed: 25 total bytes"
        );
    }

    #[test]
    fn test_request_event_display_failed() {
        let ev = RequestEvent::Failed("timeout".to_string());
        assert!(ev.to_string().contains("Request failed"));
        assert!(ev.to_string().contains("timeout"));
    }

    #[test]
    fn test_request_event_display_body_chunk() {
        let ev = RequestEvent::BodyChunk("data".to_string());
        assert_eq!(ev.to_string(), "Body chunk (4 bytes)");
    }

    #[test]
    fn test_request_event_display_temporary_problem() {
        let ev = RequestEvent::TemporaryConnectionProblem("timeout".to_string());
        assert!(ev.to_string().contains("Temporary issue"));
        assert!(ev.to_string().contains("timeout"));
    }

    // ── HeaderValue display & borrow ─────────────────────────────────────────

    #[test]
    fn test_header_value_display() {
        let v: HeaderValue = "text/html".into();
        assert_eq!(v.to_string(), "text/html");
    }

    #[test]
    fn test_header_value_borrow() {
        let v: HeaderValue = "application/json".into();
        let borrowed: &str = v.borrow();
        assert_eq!(borrowed, "application/json");
    }

    // ── JqFilter additional impls ────────────────────────────────────────────

    #[test]
    fn test_jq_filter_display() {
        let f = JqFilter::default();
        assert_eq!(f.to_string(), ".");
    }

    #[test]
    fn test_jq_filter_from_string() {
        let f: JqFilter = ".key".to_string().into();
        assert_eq!(&*f, ".key");
    }

    #[test]
    fn test_jq_filter_from_str() {
        let f: JqFilter = ".key".into();
        assert_eq!(&*f, ".key");
    }

    // ── RegexPattern additional impls ─────────────────────────────────────────

    #[test]
    fn test_regex_pattern_display() {
        let r = RegexPattern::new(r"^\w+:\s*".to_string());
        assert_eq!(r.to_string(), r"^\w+:\s*");
    }

    #[test]
    fn test_regex_pattern_deref() {
        let r = RegexPattern::new(r"\d+".to_string());
        assert_eq!(&*r, r"\d+");
    }

    #[test]
    fn test_regex_pattern_partial_eq() {
        let a = RegexPattern::new(r"^data:".to_string());
        let b = RegexPattern::new(r"^data:".to_string());
        let c = RegexPattern::new(r"^event:".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_regex_pattern_from_string() {
        let r: RegexPattern = r"^\w+:\s*".to_string().into();
        assert_eq!(r.pattern(), r"^\w+:\s*");
        assert!(r.compiled().is_some());
    }

    #[test]
    fn test_regex_pattern_from_str() {
        let r: RegexPattern = r"\d+".into();
        assert_eq!(r.pattern(), r"\d+");
        assert!(r.compiled().is_some());
    }

    // ── send_event helper ────────────────────────────────────────────────────

    #[test]
    fn test_send_event_with_sender() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        send_event(&Some(tx), RequestEvent::Started);
        let received = rx.try_recv();
        assert!(received.is_ok());
        assert!(matches!(received.unwrap(), RequestEvent::Started));
    }

    #[test]
    fn test_send_event_with_none() {
        // Must not panic when tx is None
        send_event(&None, RequestEvent::Started);
    }

    #[test]
    fn test_send_event_with_closed_channel() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        // Must not panic when channel is closed
        send_event(&Some(tx), RequestEvent::Started);
    }

    // ── fail helper ──────────────────────────────────────────────────────────

    #[test]
    fn test_fail_creates_error_and_sends_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let err = fail(
            &Some(tx),
            "bad url".to_string(),
            None,
            |msg, source| HttpError::InvalidUrl { msg, source },
        );
        assert!(matches!(err, HttpError::InvalidUrl { .. }));
        assert!(err.to_string().contains("bad url"));

        // Should have sent a Failed event
        let event = rx.try_recv();
        assert!(event.is_ok());
        assert!(matches!(event.unwrap(), RequestEvent::Failed(_)));
    }

    #[test]
    fn test_fail_with_source() {
        let inner = std::io::Error::new(std::io::ErrorKind::Other, "inner error");
        let err = fail(
            &None,
            "outer".to_string(),
            Some(Box::new(inner)),
            |msg, source| HttpError::RequestFailed { msg, source },
        );
        if let HttpError::RequestFailed { source, .. } = err {
            assert!(source.is_some());
        } else {
            panic!("expected RequestFailed");
        }
    }

    // ── Default impls & helpers ──────────────────────────────────────────────

    #[test]
    fn test_http_client_default() {
        let client = HttpClient::default();
        // just verify it doesn't panic
        let _ = client;
    }

    #[test]
    fn test_default_jq_filter_helper() {
        let f = default_jq_filter();
        assert_eq!(&*f, ".");
        assert!(is_default_jq_filter(&f));
    }

    #[test]
    fn test_is_default_jq_filter_false() {
        let f: JqFilter = ".key".into();
        assert!(!is_default_jq_filter(&f));
    }

    #[test]
    fn test_default_prefix_regex_helper() {
        let r = default_prefix_regex();
        assert_eq!(r.pattern(), r"^\w+:\s*");
        assert!(is_default_prefix_regex(&r));
    }

    #[test]
    fn test_is_default_prefix_regex_false() {
        let r = RegexPattern::new(r"^custom:".to_string());
        assert!(!is_default_prefix_regex(&r));
    }

    #[test]
    fn test_default_suffix_regex_helper() {
        let r = default_suffix_regex();
        assert_eq!(r.pattern(), r"\s*$");
        assert!(is_default_suffix_regex(&r));
    }

    #[test]
    fn test_is_default_suffix_regex_false() {
        let r = RegexPattern::new(r"###".to_string());
        assert!(!is_default_suffix_regex(&r));
    }

    // ── UrlString::into String ───────────────────────────────────────────────

    #[test]
    fn test_url_string_into_string() {
        let u = UrlString::new("https://example.com".to_string());
        let s: String = u.into();
        assert_eq!(s, "https://example.com");
    }

    // ── JqFilter::new ────────────────────────────────────────────────────────

    #[test]
    fn test_jq_filter_new() {
        let f = JqFilter::new("select(.key)".to_string());
        assert_eq!(&*f, "select(.key)");
    }

    // ── ErrorSource impls ────────────────────────────────────────────────────

    #[test]
    fn test_error_source_clone_preserves_message() {
        let inner = std::io::Error::new(std::io::ErrorKind::Other, "original msg");
        let source = ErrorSource(Box::new(inner));
        let cloned = source.clone();
        // clone is lossy — wraps to_string() in a new io::Error
        assert!(cloned.to_string().contains("original msg"));
    }

    #[test]
    fn test_error_source_source_returns_none_for_io_error() {
        let inner = std::io::Error::new(std::io::ErrorKind::Other, "test");
        let source = ErrorSource(Box::new(inner));
        // A plain io::Error has no further source
        assert!(source.source().is_none());
    }

    #[test]
    fn test_error_source_display() {
        let inner = std::io::Error::new(std::io::ErrorKind::Other, "display me");
        let source = ErrorSource(Box::new(inner));
        assert_eq!(source.to_string(), "display me");
    }

    // ── regex_pattern_serde (via non-default pattern on HttpRequest) ─────────

    #[test]
    fn test_regex_pattern_serde_custom_pattern_round_trip() {
        let mut req = HttpRequest::new(HttpMethod::GET, "https://example.com");
        req.stream_prefix_regex = RegexPattern::new(r"^custom:\s*".to_string());
        req.stream_suffix_regex = RegexPattern::new(r"END$".to_string());

        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains(r"^custom:\\s*"), "custom prefix should appear in JSON: {json}");
        assert!(json.contains(r"END$"), "custom suffix should appear in JSON: {json}");

        let decoded: HttpRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.stream_prefix_regex.pattern(), r"^custom:\s*");
        assert_eq!(decoded.stream_suffix_regex.pattern(), r"END$");
        // Ensure they're compiled after deserialization
        assert!(decoded.stream_prefix_regex.compiled().is_some());
        assert!(decoded.stream_suffix_regex.compiled().is_some());
    }
}
