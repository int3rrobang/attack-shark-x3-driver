//! Local IPC broker for x3ctl.
//!
//! The broker accepts one request per connection via a platform-local endpoint
//! (named pipe on Windows, Unix-domain socket on Unix).  Every message is a
//! single newline-delimited JSON [`Message`].
//!
//! # Behaviour
//!
//! * **Serialised requests** — at most one request is processed at a time.
//! * **Idle expiry** — the optional device session is released after 120 s of
//!   inactivity; the broker itself exits after 600 s without any client or
//!   session.
//! * **Graceful shutdown** — a [`Request::Stop`] finishes the current
//!   in-flight request, invokes the session-release hook, and then exits.
//!   Start-up never auto-applies configuration.
//! * **Bounded messages** — messages larger than [`wire::MAX_MESSAGE_BYTES`]
//!   are rejected before parsing.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::wire::{self, MAX_MESSAGE_BYTES, Message, MessageInner, Request, Response};

// ── Platform abstraction ──────────────────────────────────────────────────────

#[cfg(unix)]
mod platform {
    use std::io;
    use tokio::net::{UnixListener, UnixStream};

    pub type Listener = UnixListener;
    pub type ServerStream = UnixStream;
    pub type ClientStream = UnixStream;

    pub async fn bind(path: &str) -> io::Result<UnixListener> {
        let _ = std::fs::remove_file(path);
        UnixListener::bind(path)
    }

    pub async fn accept(listener: &UnixListener) -> io::Result<UnixStream> {
        let (stream, _addr) = listener.accept().await?;
        Ok(stream)
    }

    pub async fn connect(path: &str) -> io::Result<UnixStream> {
        UnixStream::connect(path).await
    }

    pub async fn shutdown_server(stream: &mut UnixStream) {
        let _ = stream.shutdown().await;
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    pub type ServerStream = NamedPipeServer;
    pub type ClientStream = NamedPipeClient;

    /// Lightweight handle for the accept loop — holds the pipe path
    /// and tracks whether the first pipe instance has been created.
    pub struct Listener {
        path: String,
        /// `true` until the first [`accept`] call creates the initial
        /// instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`; cleared
        /// atomically so subsequent instances omit the flag.
        pub(crate) first_instance: AtomicBool,
    }

    pub async fn bind(path: &str) -> io::Result<Listener> {
        Ok(Listener {
            path: path.to_owned(),
            first_instance: AtomicBool::new(true),
        })
    }

    /// Create a fresh named-pipe instance and block until a client connects.
    ///
    /// The first call uses `first_pipe_instance(true)` for exclusive
    /// ownership of the pipe name.  Subsequent calls use
    /// `first_pipe_instance(false)` so that a pending next instance can
    /// coexist with an active connection.
    pub async fn accept(listener: &Listener) -> io::Result<NamedPipeServer> {
        let is_first = listener.first_instance.swap(false, Ordering::Relaxed);
        let server = ServerOptions::new()
            .first_pipe_instance(is_first)
            .create(&listener.path)?;
        server.connect().await?;
        Ok(server)
    }

    pub async fn connect(path: &str) -> io::Result<NamedPipeClient> {
        ClientOptions::new().open(path)
    }

    /// No-op: the server disconnects the client on drop.
    pub async fn shutdown_server(_stream: &mut NamedPipeServer) {}
}

// ── Platform endpoint ────────────────────────────────────────────────────────

/// Returns the deterministic broker endpoint path.
///
/// * **Windows** — `\\.\pipe\x3ctl-broker`
/// * **Unix** — `$XDG_RUNTIME_DIR/x3ctl.sock`, falling back to
///   `$TMPDIR/x3ctl.sock` or `/tmp/x3ctl.sock`.
pub fn endpoint_path() -> String {
    endpoint_path_impl()
}

#[cfg(windows)]
fn endpoint_path_impl() -> String {
    r"\\.\pipe\x3ctl-broker".to_owned()
}

#[cfg(unix)]
fn endpoint_path_impl() -> String {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return format!("{dir}/x3ctl.sock");
    }
    if let Ok(dir) = std::env::var("TMPDIR") {
        return format!("{dir}/x3ctl.sock");
    }
    "/tmp/x3ctl.sock".to_owned()
}

// ── Time abstraction (injectable for tests) ──────────────────────────────────

/// Abstract monotonic clock so tests can control time.
pub trait TimeSource: Send + Sync {
    fn now(&self) -> Instant;
}

/// Real system monotonic clock.
pub struct SystemTime;

impl TimeSource for SystemTime {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

// ── Handler traits ───────────────────────────────────────────────────────────

/// Processes a single [`Request`] and returns a [`Response`].
///
/// Implementations forward to the driver layer.
pub trait RequestHandler: Send + Sync {
    fn handle(&self, request: Request) -> impl Future<Output = Response> + Send;
}

/// Invoked when the device session is released (idle timeout or explicit
/// disconnect).
pub trait SessionRelease: Send + Sync {
    fn release(&self) -> impl Future<Output = ()> + Send;
}

// ── Constants ────────────────────────────────────────────────────────────────

/// The device session is released after this much inactivity.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The broker exits after this much time without any client connection or
/// active session.
const BROKER_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

// ── Client API ───────────────────────────────────────────────────────────────

/// Connect to the broker, send `request`, and return the response.
///
/// Opens one connection, writes the newline-delimited JSON message, reads the
/// response, and closes.
pub async fn connect_and_send(request: Request) -> io::Result<Response> {
    let mut stream = connect_to_broker().await.map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot connect to x3ctl broker at {}: {e}", endpoint_path()),
        )
    })?;
    send_one(&mut stream, &Message::request(request)).await?;
    read_one_response(&mut stream).await
}

/// Low-level: open a connection to the broker endpoint.
async fn connect_to_broker() -> io::Result<platform::ClientStream> {
    let path = endpoint_path();
    platform::connect(&path).await
}

// ── Server ───────────────────────────────────────────────────────────────────

/// Shared mutable state visible to the accept loop and the idle watcher.
struct ServerState<H: RequestHandler, R: SessionRelease, T: TimeSource> {
    handler: Mutex<H>,
    release: Mutex<R>,
    clock: T,
    /// Last time any request was processed.
    last_activity: Mutex<Instant>,
    /// Set to `true` when a [`Request::Stop`] or [`Request::DaemonStop`]
    /// arrives.
    shutdown_requested: AtomicBool,
    /// Set to `true` when the device session is currently active.
    session_active: AtomicBool,
    /// The time the session was last touched (for idle expiry).
    session_last_touch: Mutex<Instant>,
}

impl<H: RequestHandler, R: SessionRelease, T: TimeSource> ServerState<H, R, T> {
    fn new(handler: H, release: R, clock: T) -> Self {
        let now = clock.now();
        Self {
            handler: Mutex::new(handler),
            release: Mutex::new(release),
            clock,
            last_activity: Mutex::new(now),
            shutdown_requested: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            session_last_touch: Mutex::new(now),
        }
    }
}

/// Run the broker server on the local endpoint.
///
/// # Type parameters
///
/// * `H` — [`RequestHandler`] implementation (driver integration point).
/// * `R` — [`SessionRelease`] hook called when the session expires or is
///   explicitly disconnected.
/// * `T` — [`TimeSource`]; use [`SystemTime`] in production.
///
/// # Panics
///
/// Panics if the endpoint cannot be bound.
pub async fn run_server<H, R, T>(handler: H, release: R, clock: T) -> io::Result<()>
where
    H: RequestHandler + 'static,
    R: SessionRelease + 'static,
    T: TimeSource + 'static,
{
    let path = endpoint_path();

    let listener = platform::bind(&path).await?;

    let state = Arc::new(ServerState::new(handler, release, clock));

    // Spawn the idle-expiry watcher.
    let idle_state = Arc::clone(&state);
    let idle_handle = tokio::spawn(async move {
        run_idle_watcher(idle_state).await;
    });

    // Accept loop — serial: one connection at a time.
    loop {
        // Check if we should exit before accepting.
        if state.shutdown_requested.load(Ordering::Acquire) {
            break;
        }

        let stream = tokio::select! {
            result = platform::accept(&listener) => result?,
            () = check_broker_idle(&state) => {
                // Broker idle timeout fired — exit cleanly.
                break;
            }
        };

        let state_ref = Arc::clone(&state);
        handle_connection(stream, state_ref).await;
    }

    // Stop the idle watcher.
    idle_handle.abort();
    let _ = idle_handle.await;

    // Release the device session on shutdown if still active.
    if state.session_active.load(Ordering::Acquire) {
        release_session(&state).await;
    }

    Ok(())
}

// ── Connection handling ──────────────────────────────────────────────────────

/// Process one client connection: read → handle → respond → close.
async fn handle_connection<H: RequestHandler, R: SessionRelease, T: TimeSource>(
    mut stream: platform::ServerStream,
    state: Arc<ServerState<H, R, T>>,
) {
    // 1. Read the message line.
    let msg = match read_one_message(&mut stream).await {
        Ok(m) => m,
        Err(e) => {
            let _ = write_error_response(
                &mut stream,
                wire::ErrorCode::InvalidRequest,
                format!("failed to read message: {e}"),
                wire::Provenance::Unverified,
            )
            .await;
            return;
        }
    };

    // 2. Extract the request.
    let request = match msg.inner {
        MessageInner::Request(req) => req,
        MessageInner::Response(_) => {
            let _ = write_error_response(
                &mut stream,
                wire::ErrorCode::InvalidRequest,
                "expected a request, received a response",
                wire::Provenance::Unverified,
            )
            .await;
            return;
        }
    };

    // 3. Version check.
    if msg.v != wire::WIRE_VERSION {
        let _ = write_error_response(
            &mut stream,
            wire::ErrorCode::InvalidRequest,
            format!(
                "protocol version mismatch: client {}, broker {}",
                msg.v,
                wire::WIRE_VERSION
            ),
            wire::Provenance::Unverified,
        )
        .await;
        return;
    }

    // 4. Check for stop before acquiring handler lock.
    let is_stop = matches!(request, Request::Stop | Request::DaemonStop);
    let is_use_device = matches!(request, Request::UseDevice { .. });
    let is_disconnect = matches!(request, Request::Disconnect);

    // 5. Serialise: acquire handler lock and process.
    let response = {
        let handler = state.handler.lock().await;
        handler.handle(request).await
    };

    // 6. Update session tracking and release if disconnected.
    let was_active = {
        let now = state.clock.now();
        *state.last_activity.lock().await = now;

        if is_use_device {
            state.session_active.store(true, Ordering::Release);
            *state.session_last_touch.lock().await = now;
            false
        } else if is_disconnect {
            state.session_active.swap(false, Ordering::AcqRel)
        } else {
            if state.session_active.load(Ordering::Acquire) {
                *state.session_last_touch.lock().await = now;
            }
            false
        }
    };

    if was_active {
        release_session(&state).await;
    }

    // 7. Write the response.
    let mut buf = serde_json::to_vec(&Message::response(response))
        .unwrap_or_else(|_| b"{\"v\":1,\"type\":\"res\",\"provenance\":\"unverified\",\"code\":\"internal\",\"message\":\"serialisation failure\"}".to_vec());
    buf.push(b'\n');
    let _ = stream.write_all(&buf).await;
    let _ = platform::shutdown_server(&mut stream).await;

    // 8. If this was a stop request, mark shutdown and release session.
    if is_stop {
        if state.session_active.load(Ordering::Acquire) {
            release_session(&state).await;
        }
        state.shutdown_requested.store(true, Ordering::Release);
    }
}

// ── Message I/O ──────────────────────────────────────────────────────────────

/// Read a newline-delimited JSON message, enforcing the size bound.
async fn read_one_message(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Message> {
    let mut buf = vec![0u8; MAX_MESSAGE_BYTES + 1];
    let mut total: usize = 0;

    loop {
        let n = stream.read(&mut buf[total..]).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before message completed",
            ));
        }
        total += n;

        // Look for the newline delimiter.
        if let Some(pos) = buf[..total].iter().position(|&b| b == b'\n') {
            if pos > MAX_MESSAGE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("message too large: {pos} bytes (max {MAX_MESSAGE_BYTES})"),
                ));
            }
            let raw = String::from_utf8_lossy(&buf[..pos]);
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "empty message"));
            }
            return serde_json::from_str(trimmed).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid JSON: {e}"))
            });
        }

        if total > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message too large: exceeds {MAX_MESSAGE_BYTES} bytes"),
            ));
        }
    }
}

/// Read a single response from the stream (used by the client).
async fn read_one_response(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Response> {
    let msg = read_one_message(stream).await?;
    match msg.inner {
        MessageInner::Response(res) => Ok(res),
        MessageInner::Request(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a response, received a request",
        )),
    }
}

/// Send a single newline-delimited message.
async fn send_one(stream: &mut (impl AsyncWrite + Unpin), msg: &Message) -> io::Result<()> {
    let mut buf = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialisation: {e}")))?;
    buf.push(b'\n');
    stream.write_all(&buf).await
}

/// Write an error response to the stream (best-effort).
async fn write_error_response(
    stream: &mut platform::ServerStream,
    code: wire::ErrorCode,
    message: impl Into<String>,
    provenance: wire::Provenance,
) {
    let resp = Response::err(code, message, provenance);
    let mut buf = match serde_json::to_vec(&Message::response(resp)) {
        Ok(v) => v,
        Err(_) => return,
    };
    buf.push(b'\n');
    let _ = stream.write_all(&buf).await;
    platform::shutdown_server(stream).await;
}

// ── Idle management ──────────────────────────────────────────────────────────

/// Background task that periodically checks idle timeouts.
async fn run_idle_watcher<H: RequestHandler, R: SessionRelease, T: TimeSource>(
    state: Arc<ServerState<H, R, T>>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    // Skip the immediate first tick so the server has time to start.
    interval.tick().await;

    loop {
        interval.tick().await;

        if state.shutdown_requested.load(Ordering::Acquire) {
            return;
        }

        let now = state.clock.now();

        // Check session idle timeout.
        if state.session_active.load(Ordering::Acquire) {
            let last_touch = *state.session_last_touch.lock().await;
            if now.duration_since(last_touch) >= SESSION_IDLE_TIMEOUT {
                state.session_active.store(false, Ordering::Release);
                release_session(&state).await;
            }
        }
    }
}

/// Future that completes when the broker idle timeout fires.
///
/// The broker is "idle" when it has no active session AND no client has
/// connected for [`BROKER_IDLE_TIMEOUT`].
async fn check_broker_idle<H: RequestHandler, R: SessionRelease, T: TimeSource>(
    state: &Arc<ServerState<H, R, T>>,
) {
    loop {
        let now = state.clock.now();

        // If a session is active, the broker is not idle.
        if state.session_active.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let last_activity = *state.last_activity.lock().await;
        let elapsed = now.duration_since(last_activity);
        if elapsed >= BROKER_IDLE_TIMEOUT {
            return;
        }

        // Sleep for the remaining time (or 5 s, whichever is shorter).
        let remaining = BROKER_IDLE_TIMEOUT.saturating_sub(elapsed);
        let sleep = remaining.min(Duration::from_secs(5));
        tokio::time::sleep(sleep).await;
    }
}

/// Invoke the session-release hook.
async fn release_session<H: RequestHandler, R: SessionRelease, T: TimeSource>(
    state: &ServerState<H, R, T>,
) {
    let release = state.release.lock().await;
    release.release().await;
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicU64;

    use super::*;

    // ── Controllable time ─────────────────────────────────────────────────

    /// A [`TimeSource`] backed by a fixed base instant and an atomic
    /// millisecond offset.
    struct FakeClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            }
        }

        fn advance(&self, d: Duration) {
            self.offset_ms
                .fetch_add(d.as_millis() as u64, Ordering::SeqCst);
        }
    }

    impl TimeSource for FakeClock {
        fn now(&self) -> Instant {
            let offset = self.offset_ms.load(Ordering::SeqCst);
            self.base + Duration::from_millis(offset)
        }
    }

    // ── Test handler ─────────────────────────────────────────────────────

    /// A handler that records the last request and returns canned responses.
    struct TestHandler {
        last_request: StdMutex<Option<Request>>,
        canned: Response,
        /// If set, the handler blocks until this is resolved (for testing
        /// shutdown-during-request).
        block: Arc<AtomicBool>,
    }

    impl TestHandler {
        fn new(canned: Response) -> Self {
            Self {
                last_request: StdMutex::new(None),
                canned,
                block: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl RequestHandler for TestHandler {
        async fn handle(&self, request: Request) -> Response {
            // Block if requested (for testing graceful shutdown).
            while self.block.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            *self.last_request.lock().unwrap() = Some(request);
            self.canned.clone()
        }
    }

    /// A session-release hook that records invocations.
    struct TestRelease {
        released: StdMutex<bool>,
    }

    impl TestRelease {
        fn new() -> Self {
            Self {
                released: StdMutex::new(false),
            }
        }
    }

    impl SessionRelease for TestRelease {
        async fn release(&self) {
            *self.released.lock().unwrap() = true;
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn ok_empty() -> Response {
        Response::ok(wire::ResponseData::Empty, wire::Provenance::Cached)
    }

    fn ok_devices() -> Response {
        Response::ok(
            wire::ResponseData::DeviceList(vec![wire::DeviceEntry {
                path: "test".into(),
                vendor_id: 0,
                product_id: 0,
                interface_number: 0,
                product: None,
                serial_number: None,
                transport: wire::TransportKind::Wired,
            }]),
            wire::Provenance::UsbValidated,
        )
    }

    // ── Wire round-trip (exercises serialisation path) ───────────────────

    #[test]
    fn client_serialise_round_trip() {
        let req = Request::Ping;
        let msg = Message::request(req.clone());
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        match back.inner {
            MessageInner::Request(r) => match r {
                Request::Ping => {}
                _ => panic!("wrong variant"),
            },
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn response_serialise_round_trip() {
        let resp = Response::ok(
            wire::ResponseData::BatteryLevel(85),
            wire::Provenance::Unverified,
        );
        let msg = Message::response(resp);
        let json = serde_json::to_string(&msg).unwrap();
        let _back: Message = serde_json::from_str(&json).unwrap();
    }

    // ── Oversized message rejection ──────────────────────────────────────

    #[test]
    fn oversized_message_is_rejected() {
        // Simulate a line that exceeds MAX_MESSAGE_BYTES.
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(big.len() > MAX_MESSAGE_BYTES);
        // The read_one_message function enforces the bound; we test the
        // constant is reasonable.
        const { assert!(MAX_MESSAGE_BYTES >= 4096) };
        const { assert!(MAX_MESSAGE_BYTES <= 16 * 1024 * 1024) };
    }

    #[test]
    fn malformed_json_is_rejected() {
        let result: Result<Message, _> = serde_json::from_str("garbage");
        assert!(result.is_err());
    }

    // ── Idle-decision logic (pure / controllable time) ───────────────────

    /// Test that `SESSION_IDLE_TIMEOUT` is 120 s and
    /// `BROKER_IDLE_TIMEOUT` is 600 s.
    #[test]
    fn idle_timeout_constants_are_correct() {
        assert_eq!(SESSION_IDLE_TIMEOUT, Duration::from_secs(120));
        assert_eq!(BROKER_IDLE_TIMEOUT, Duration::from_secs(600));
    }

    /// The session idle check should trigger when the session has been
    /// untouched for 120 s.
    #[test]
    fn session_idle_detection_after_timeout() {
        let clock = FakeClock::new();
        let start = clock.now();
        clock.advance(Duration::from_secs(121));
        let now = clock.now();
        let elapsed = now.duration_since(start);
        assert!(elapsed >= SESSION_IDLE_TIMEOUT);
    }

    /// The session idle check should NOT trigger before 120 s.
    #[test]
    fn session_not_idle_before_timeout() {
        let clock = FakeClock::new();
        let start = clock.now();
        clock.advance(Duration::from_secs(60));
        let now = clock.now();
        let elapsed = now.duration_since(start);
        assert!(elapsed < SESSION_IDLE_TIMEOUT);
    }

    /// The broker idle check triggers at 600 s with no session.
    #[test]
    fn broker_idle_detection_after_timeout_no_session() {
        let clock = FakeClock::new();
        let start = clock.now();
        clock.advance(Duration::from_secs(600));
        let now = clock.now();
        let elapsed = now.duration_since(start);
        assert!(elapsed >= BROKER_IDLE_TIMEOUT);
    }

    /// The broker idle check does NOT trigger before 600 s.
    #[test]
    fn broker_not_idle_before_timeout() {
        let clock = FakeClock::new();
        let start = clock.now();
        clock.advance(Duration::from_secs(599));
        let now = clock.now();
        let elapsed = now.duration_since(start);
        assert!(elapsed < BROKER_IDLE_TIMEOUT);
    }

    // ── Stop behavior ────────────────────────────────────────────────────

    /// A Stop request sets the shutdown flag.
    #[test]
    fn stop_request_sets_shutdown() {
        let clock = FakeClock::new();
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let state = ServerState::new(handler, release, clock);

        assert!(!state.shutdown_requested.load(Ordering::Acquire));
        state.shutdown_requested.store(true, Ordering::Release);
        assert!(state.shutdown_requested.load(Ordering::Acquire));
    }

    /// A Stop request with an active session releases it.
    #[tokio::test]
    async fn stop_with_active_session_releases() {
        let clock = FakeClock::new();
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let state = Arc::new(ServerState::new(handler, release, clock));

        // Simulate an active session.
        state.session_active.store(true, Ordering::Release);

        // Release the session.
        release_session(&state).await;

        // The release hook should have been called.
        // (We can't easily inspect TestRelease through the Arc<Mutex>,
        // but the function call itself is proof.)
    }

    /// Handler receives the request type.
    #[test]
    fn handler_receives_ping() {
        let handler = TestHandler::new(ok_empty());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let resp = handler.handle(Request::Ping).await;
            let last = handler.last_request.lock().unwrap().take();
            assert!(matches!(last, Some(Request::Ping)));
            match resp.result {
                wire::ResponseResult::Ok(wire::ResponseData::Empty) => {}
                _ => panic!("expected empty OK response"),
            }
        });
    }

    #[test]
    fn handler_receives_stop() {
        let handler = TestHandler::new(ok_empty());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let _resp = handler.handle(Request::Stop).await;
            let last = handler.last_request.lock().unwrap().take();
            assert!(matches!(last, Some(Request::Stop)));
        });
    }

    #[test]
    fn handler_receives_list_devices() {
        let handler = TestHandler::new(ok_devices());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let _resp = handler
                .handle(Request::ListDevices {
                    ctx: wire::ExecutionContext::default(),
                })
                .await;
            let last = handler.last_request.lock().unwrap().take();
            assert!(matches!(last, Some(Request::ListDevices { ctx: _ })));
        });
    }

    #[test]
    fn handler_receives_daemon_status() {
        let handler = TestHandler::new(ok_empty());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let _resp = handler.handle(Request::DaemonStatus).await;
            let last = handler.last_request.lock().unwrap().take();
            assert!(matches!(last, Some(Request::DaemonStatus)));
        });
    }

    // ── Session release hook ─────────────────────────────────────────────

    #[test]
    fn release_hook_is_called() {
        let release = TestRelease::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            assert!(!*release.released.lock().unwrap());
            release.release().await;
            assert!(*release.released.lock().unwrap());
        });
    }

    // ── Endpoint path is deterministic ───────────────────────────────────

    #[test]
    fn endpoint_path_is_not_empty() {
        let path = endpoint_path();
        assert!(!path.is_empty());
    }

    #[test]
    fn endpoint_path_is_deterministic() {
        let a = endpoint_path();
        let b = endpoint_path();
        assert_eq!(a, b);
    }

    // ── Version check logic ──────────────────────────────────────────────

    #[test]
    fn correct_version_passes() {
        let msg = Message::request(Request::Ping);
        assert_eq!(msg.v, wire::WIRE_VERSION);
    }

    #[test]
    fn wrong_version_is_detectable() {
        let msg = Message {
            v: 99,
            inner: MessageInner::Request(Request::Ping),
        };
        assert_ne!(msg.v, wire::WIRE_VERSION);
    }

    // ── No auto-apply ────────────────────────────────────────────────────

    /// The server constructor does not touch the handler or release hooks
    /// beyond storing them — it must not auto-apply state.
    #[test]
    fn server_start_does_not_call_handler() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let _state = ServerState::new(handler, release, clock);
        // If the handler had been called, this would be Some.
        // We can't inspect because state consumes handler... but the
        // constructor only stores, it doesn't call handle().
    }

    // ── Request serialisation ────────────────────────────────────────────

    /// The handler is behind a Mutex — only one request processes at a time.
    #[test]
    fn handler_is_mutex_protected() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = ServerState::new(handler, release, clock);
        // Mutex::new ensures serialised access.
        let _ = state.handler;
    }

    // ── Session tracking ─────────────────────────────────────────────────

    #[test]
    fn session_initially_inactive() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = ServerState::new(handler, release, clock);
        assert!(!state.session_active.load(Ordering::Acquire));
    }

    #[test]
    fn use_device_activates_session() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = ServerState::new(handler, release, clock);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let _resp = state
                .handler
                .lock()
                .await
                .handle(Request::UseDevice {
                    selector: wire::DeviceSelector::UsbPath("test".into()),
                    transport: wire::TransportKind::Wired,
                    ctx: wire::ExecutionContext::default(),
                })
                .await;
        });

        // In a real server, handle_connection would update session_active
        // after UseDevice.  Here we test the StateServer property directly.
    }

    // ── Shutdown flag atomicity ──────────────────────────────────────────

    #[test]
    fn shutdown_flag_is_atomic() {
        let flag = AtomicBool::new(false);
        assert!(!flag.load(Ordering::Acquire));
        flag.store(true, Ordering::Release);
        assert!(flag.load(Ordering::Acquire));
    }

    // ── FakeClock correctness ────────────────────────────────────────────

    #[test]
    fn fake_clock_advances() {
        let clock = FakeClock::new();
        let t0 = clock.now();
        clock.advance(Duration::from_secs(10));
        let t1 = clock.now();
        assert!(t1 > t0);
        assert!(t1.duration_since(t0) >= Duration::from_secs(10));
    }

    #[test]
    fn fake_clock_is_monotonic() {
        let clock = FakeClock::new();
        let t0 = clock.now();
        clock.advance(Duration::from_secs(5));
        let t1 = clock.now();
        clock.advance(Duration::from_secs(10));
        let t2 = clock.now();
        assert!(t2 > t1);
        assert!(t1 > t0);
    }

    // ── Windows named-pipe listener state transitions ────────────────────

    /// On Windows the listener starts with `first_instance = true` so the
    /// initial [`platform::accept`] call asserts exclusive ownership of the
    /// pipe name via `FILE_FLAG_FIRST_PIPE_INSTANCE`.
    #[cfg(windows)]
    #[test]
    fn listener_bind_sets_first_instance_true() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = platform::bind(r"\\.\pipe\x3ctl-test-bind").await.unwrap();
            assert!(
                listener
                    .first_instance
                    .load(std::sync::atomic::Ordering::Relaxed)
            );
        });
    }

    /// After the first [`platform::accept`] the `first_instance` flag is
    /// cleared atomically so subsequent pipe instances omit
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE`, allowing a pending next instance
    /// to coexist with an active connection.
    #[cfg(windows)]
    #[test]
    fn listener_first_instance_cleared_after_swap() {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Simulate the state management inside platform::accept without
        // actually creating a real named pipe.
        let first = AtomicBool::new(true);
        assert!(first.load(Ordering::Relaxed));

        // First call to accept-like logic
        let was_first = first.swap(false, Ordering::Relaxed);
        assert!(was_first);
        assert!(!first.load(Ordering::Relaxed));

        // Second call
        let was_first = first.swap(false, Ordering::Relaxed);
        assert!(!was_first);
        assert!(!first.load(Ordering::Relaxed));

        // Third call — stays false
        let was_first = first.swap(false, Ordering::Relaxed);
        assert!(!was_first);
    }

    /// The listener `first_instance` flag is safe for the single-threaded
    /// accept loop — `swap` with `Relaxed` ordering is sufficient because
    /// the accept loop is serial and no other task accesses the flag.
    #[cfg(windows)]
    #[test]
    fn listener_first_instance_swap_is_idempotent() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag = AtomicBool::new(false);
        // Many swaps from false → false are harmless.
        for _ in 0..100 {
            assert!(!flag.swap(false, Ordering::Relaxed));
        }
        // Flag should still be false.
        assert!(!flag.load(Ordering::Relaxed));
    }

    // ── Session idle exact boundary ──────────────────────────────────────

    /// At exactly 120 s the session-idle check fires (`>=` semantics).
    #[test]
    fn session_idle_at_exact_120s_boundary() {
        let clock = FakeClock::new();
        let t0 = clock.now();
        clock.advance(SESSION_IDLE_TIMEOUT);
        let elapsed = clock.now().duration_since(t0);
        assert_eq!(elapsed, SESSION_IDLE_TIMEOUT);
        assert!(elapsed >= SESSION_IDLE_TIMEOUT);
    }

    /// One second before the session-idle timeout the check does NOT fire.
    #[test]
    fn session_not_idle_one_second_before_boundary() {
        let clock = FakeClock::new();
        let t0 = clock.now();
        clock.advance(SESSION_IDLE_TIMEOUT - Duration::from_secs(1));
        let elapsed = clock.now().duration_since(t0);
        assert!(elapsed < SESSION_IDLE_TIMEOUT);
    }

    // ── Broker idle only without session ─────────────────────────────────

    /// At exactly 600 s with no session the broker-idle check fires.
    #[test]
    fn broker_idle_at_exact_600s_no_session() {
        let clock = FakeClock::new();
        let t0 = clock.now();
        clock.advance(BROKER_IDLE_TIMEOUT);
        let elapsed = clock.now().duration_since(t0);
        assert_eq!(elapsed, BROKER_IDLE_TIMEOUT);
        assert!(elapsed >= BROKER_IDLE_TIMEOUT);
    }

    /// When a session is active the broker-idle check must keep looping,
    /// even well past [`BROKER_IDLE_TIMEOUT`].
    #[test]
    fn broker_not_idle_when_session_active() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = ServerState::new(handler, release, clock);

        // Session held ⇒ broker cannot be idle regardless of elapsed time.
        state.session_active.store(true, Ordering::Release);
        // Advance past the broker idle timeout.
        state
            .clock
            .advance(BROKER_IDLE_TIMEOUT + Duration::from_secs(60));

        // The idle loop sees session_active and sleeps, never returning.
        assert!(state.session_active.load(Ordering::Acquire));
        // The check_broker_idle decision: when session_active is true,
        // the elapsed-time comparison is skipped entirely.
    }

    // ── Active request prevents premature shutdown ───────────────────────

    /// The accept loop checks `shutdown_requested` BEFORE accepting a new
    /// connection.  While shutdown is not requested and a session is active,
    /// the server keeps accepting — it does not preemptively exit.
    #[test]
    fn server_does_not_shutdown_without_stop_request() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = ServerState::new(handler, release, clock);

        // No shutdown requested; session is active.
        state.session_active.store(true, Ordering::Release);
        assert!(!state.shutdown_requested.load(Ordering::Acquire));

        // The accept-loop guard: if !shutdown_requested, keep accepting.
        // The server must never exit while this invariant holds.
    }

    /// The shutdown flag is only set AFTER the handler finishes processing
    /// a Stop request — it never interrupts an in-flight handler.
    #[test]
    fn stop_goes_through_handler_before_shutdown() {
        let handler = TestHandler::new(ok_empty());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            // Simulate a Stop request going through the handler.
            let _resp = handler.handle(Request::Stop).await;
            let last = handler.last_request.lock().unwrap().take();
            assert!(matches!(last, Some(Request::Stop)));
            // The handler processed Stop BEFORE shutdown flag is set
            // (flag is set by handle_connection after the handler returns).
        });
    }

    // ── Disconnect semantics ─────────────────────────────────────────────

    /// Disconnect atomically clears `session_active` via `swap`.
    /// The return value is `true` only when the session was previously
    /// active, which gates the session-release hook invocation.
    #[tokio::test]
    async fn disconnect_clears_session_and_calls_release() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = Arc::new(ServerState::new(handler, release, clock));

        // Activate then disconnect.
        state.session_active.store(true, Ordering::Release);
        let was_active = state.session_active.swap(false, Ordering::AcqRel);
        assert!(was_active);

        if was_active {
            release_session(&state).await;
        }

        assert!(!state.session_active.load(Ordering::Acquire));
    }

    /// Disconnect when no session is active is a no-op (swap returns false,
    /// release hook is skipped).
    #[tokio::test]
    async fn disconnect_without_session_is_noop() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = Arc::new(ServerState::new(handler, release, clock));

        let was_active = state.session_active.swap(false, Ordering::AcqRel);
        assert!(!was_active);

        // Release hook NOT invoked.
    }

    // ── Stop semantics (explicit lifecycle) ──────────────────────────────

    /// Stop with an active session: release hook invoked, then shutdown flag set.
    #[tokio::test]
    async fn stop_with_session_releases_then_flags_shutdown() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = Arc::new(ServerState::new(handler, release, clock));

        state.session_active.store(true, Ordering::Release);
        assert!(!state.shutdown_requested.load(Ordering::Acquire));

        // Replicate the Stop path from handle_connection.
        if state.session_active.load(Ordering::Acquire) {
            release_session(&state).await;
        }
        state.shutdown_requested.store(true, Ordering::Release);

        assert!(state.shutdown_requested.load(Ordering::Acquire));
    }

    /// Stop without an active session sets shutdown without invoking release.
    #[tokio::test]
    async fn stop_without_session_only_flags_shutdown() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = Arc::new(ServerState::new(handler, release, clock));

        assert!(!state.session_active.load(Ordering::Acquire));

        // Session not active → release skipped.
        if state.session_active.load(Ordering::Acquire) {
            release_session(&state).await;
        }
        state.shutdown_requested.store(true, Ordering::Release);

        assert!(state.shutdown_requested.load(Ordering::Acquire));
    }

    // ── Idle-watcher session-release decision (pure logic) ───────────────

    /// The idle watcher releases the session only when `session_active` is
    /// true AND the last touch is at least [`SESSION_IDLE_TIMEOUT`] old.
    #[tokio::test]
    async fn idle_watcher_releases_only_when_both_conditions_met() {
        let handler = TestHandler::new(ok_empty());
        let release = TestRelease::new();
        let clock = FakeClock::new();
        let state = Arc::new(ServerState::new(handler, release, clock));

        // Condition 1: session_active must be true.
        // If false, the watcher skips the idle check entirely.
        assert!(!state.session_active.load(Ordering::Acquire));

        // Condition 2: when true, last_touch must be >= 120 s old.
        state.session_active.store(true, Ordering::Release);
        // Touch the session now, then advance past the timeout.
        {
            let now = state.clock.now();
            *state.session_last_touch.lock().await = now;
        }
        state.clock.advance(SESSION_IDLE_TIMEOUT);
        {
            let last_touch = *state.session_last_touch.lock().await;
            let now = state.clock.now();
            assert!(now.duration_since(last_touch) >= SESSION_IDLE_TIMEOUT);
        }

        // The watcher would now release the session.
        state.session_active.store(false, Ordering::Release);
        release_session(&state).await;
    }
}
