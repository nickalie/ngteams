//! Typed reads and requests over a transport. Writes are sent without permits.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use crate::Mri;

use crate::http::{Credentials, HttpError, HttpTransport};
use crate::own::OwnState;
use crate::post_read::PostRead;
use crate::read::Read;
use crate::request::Request;

/// Sends typed reads and requests. Delivery exposes HTTP statuses to the caller for retry decisions.
pub trait Transport: Sync {
    type Error: Error + Send + Sync + 'static;

    /// The account's service boundary, also used by its push channel. Microsoft is the default.
    fn cloud(&self) -> &crate::Cloud {
        &crate::Cloud::Microsoft
    }

    /// A typed call-scoped operation on Microsoft's built-in caption service.
    fn deliver_caption(
        &self,
        write: crate::captions::CaptionWrite,
    ) -> impl Future<Output = Option<Result<Response, Self::Error>>> + Send {
        let _ = write;
        async { None }
    }

    /// A typed own-drive attachment operation, issued only by the outbox after its operational preflight.
    /// A transport without this route answers `None` and sends nothing.
    fn deliver_attachment(
        &self,
        write: crate::DriveWrite,
    ) -> impl Future<Output = Option<Result<Response, Self::Error>>> + Send {
        let _ = write;
        async { None }
    }

    /// A typed Power Automate operation. Only the outbox issues mutations; unsupported transports fail closed.
    fn workflow(&self, request: &crate::WorkflowRequest) -> impl Future<Output = crate::WorkflowResponse> + Send {
        let _ = request;
        async { Err(crate::WorkflowFailure::Connection) }
    }

    /// Sends a read as a `GET`.
    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    fn send<R: Request>(&self, request: &R) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// Account-owned channel preferences and folder updates, with an independent native actor check.
    fn channel_own<R: crate::ChannelOwn + Sync>(
        &self,
        request: &R,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send {
        self.send(request)
    }

    /// Sends an account-owned protocol request through the transport.
    fn send_own<R: OwnState + Request + Sync>(
        &self,
        request: &R,
    ) -> impl Future<Output = Result<Response, Self::Error>> + Send {
        self.send(request)
    }

    /// Sends a typed write and returns the service's answer, whatever its status: what to do about a refusal is
    /// the caller's (the outbox's) decision.
    fn deliver<R: Request + Send>(&self, write: R) -> impl Future<Output = Result<Response, Self::Error>> + Send;

    /// Whether this transport sends only reads (the personal build's read-only mode, [`HttpTransport::read_only`]):
    /// `false` (the default) for one that may write.
    fn is_read_only(&self) -> bool {
        false
    }

    fn read_only_refusal<R: Request>(&self, write: &R) -> Option<Self::Error> {
        let _ = write;
        None
    }

    fn own_account(&self) -> Option<&Mri> {
        None
    }
}

/// A service's answer: its status, the few headers a caller acts on, and the body (capped by the transport that read
/// it). `Debug` shows the body's length only: a body may hold message text.
#[derive(Clone, PartialEq, Eq)]
pub struct Response {
    status: u16,
    location: Option<String>,
    retry_after: Option<Duration>,
    content_type: Option<String>,
    content_range: Option<String>,
    body: Vec<u8>,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("location", &self.location)
            .field("retry_after", &self.retry_after)
            .field("content_type", &self.content_type)
            .field("content_range", &self.content_range)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Response {
    /// An answer with `status` and `body` and none of the headers.
    #[must_use]
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            location: None,
            retry_after: None,
            content_type: None,
            content_range: None,
            body: body.into(),
        }
    }

    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    #[must_use]
    pub fn with_retry_after(mut self, wait: Duration) -> Self {
        self.retry_after = Some(wait);
        self
    }

    #[must_use]
    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    #[must_use]
    pub fn with_content_range(mut self, content_range: impl Into<String>) -> Self {
        self.content_range = Some(content_range.into());
        self
    }

    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// A 2xx status.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Location`: where a message post's new message is, or where a region hop points (never followed).
    #[must_use]
    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    /// `Retry-After`, as a wait from when the answer was read.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// `Content-Range`: which part of a file a ranged read's answer holds.
    #[must_use]
    pub fn content_range(&self) -> Option<&str> {
        self.content_range.as_deref()
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The body, taken.
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

/// Teams' internal API over a [`Transport`].
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
}

impl<T: Transport> Client<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Sends a read.
    ///
    /// # Errors
    /// The transport's.
    pub async fn read<R: Read>(&self, request: &R) -> Result<Response, T::Error> {
        self.transport.read(request).await
    }

    /// Sends the typed request.
    ///
    /// # Errors
    /// Returns the transport error when the request cannot complete.
    pub async fn send<R: Request>(&self, request: &R) -> Result<Response, T::Error> {
        self.transport.send(request).await
    }

    /// Sends the typed request.
    ///
    /// # Errors
    /// Returns the transport error when the request cannot complete.
    pub async fn write<R: Request + Send>(&self, request: R) -> Result<Response, T::Error> {
        self.transport.deliver(request).await
    }
}

impl<C: Credentials> Client<HttpTransport<C>> {
    /// Sends an entry of the sealed own-state list ([`HttpTransport::own_state`]).
    ///
    /// # Errors
    /// The transport's.
    pub async fn own_state<R: OwnState>(&self, request: &R) -> Result<Response, HttpError> {
        self.transport.own_state(request).await
    }

    /// Sends the typed request.
    ///
    /// # Errors
    /// Returns the transport error when the request cannot complete.
    pub async fn lookup<R: PostRead + Send + Sync>(&self, request: R) -> Result<Response, HttpError> {
        self.transport.post_read(request).await
    }

    /// Sends a typed read by POST without a permit.
    ///
    /// # Errors
    /// The transport's.
    pub async fn post_read<R: PostRead + Send + Sync>(&self, request: R) -> Result<Response, HttpError> {
        self.transport.post_read(request).await
    }
}
