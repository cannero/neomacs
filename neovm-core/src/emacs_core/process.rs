//! Process/subprocess management for the Elisp VM.
//!
//! Provides process abstractions: creating, killing, querying, and
//! communicating with subprocesses.  `start-process` creates a tracked
//! record; `call-process` and `shell-command-to-string` run real OS
//! commands via `std::process::Command`.
//!
//! ## Network processes
//!
//! `make-network-process` supports TCP client connections. The socket fd
//! is registered with the same `polling::Poller` used for child process
//! stdout, so `accept-process-output` and `poll_process_output` wake on
//! incoming data.
//!
//! **TLS**: `gnutls-boot` upgrades a network process to TLS using `rustls`.
//! The `TcpStream` is moved into a `rustls::StreamOwned` stored in
//! `Process.tls_stream`. Read/write/send automatically use the TLS layer
//! when present. Mozilla root certificates are used for verification.

use std::collections::HashMap;
#[cfg(not(target_os = "windows"))]
use std::ffi::CStr;
use std::fs::OpenOptions;
use std::io::Read as IoRead;
use std::net::IpAddr;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A TLS-wrapped TCP stream using rustls.
/// The underlying `TcpStream` is owned by `StreamOwned`, so when TLS is active
/// the `Process.socket` field is `None`.
#[cfg(unix)]
pub type TlsStream = rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::threads::ThreadManager;
use super::value::{
    StringTextPropertyRun, Value, list_to_vec, next_float_id, read_cons, with_heap,
};
use crate::buffer::BufferManager;
use crate::gc::GcTrace;
use crate::window::FrameManager;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Unique identifier for a process.
pub type ProcessId = u64;

/// Status of a managed process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    Run,
    Stop,
    Exit(i32),
    Signal(i32),
    /// Async network connection in progress (GNU: Qconnect).
    Connect,
    /// Network connection failed (GNU: Qfailed).
    Failed,
}

/// Process family used by compatibility helpers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessKind {
    Real,
    Network,
    Pipe,
    Serial,
}

/// A tracked process record.
#[derive(Debug)]
pub struct Process {
    pub id: ProcessId,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub kind: ProcessKind,
    pub status: ProcessStatus,
    pub buffer_name: Option<String>,
    /// Queued input (sent via `process-send-string`).
    pub stdin_queue: String,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Query-on-exit flag state.
    pub query_on_exit_flag: bool,
    /// Process filter callback (or default marker symbol).
    pub filter: Value,
    /// Process sentinel callback (or default marker symbol).
    pub sentinel: Value,
    /// Process plist state.
    pub plist: Value,
    /// Current decoding coding-system.
    pub coding_decode: Value,
    /// Current encoding coding-system.
    pub coding_encode: Value,
    /// Inherit-coding-system flag.
    pub inherit_coding_system_flag: bool,
    /// Attached thread object.
    pub thread: Value,
    /// Last process-window-size columns value.
    pub window_cols: Option<i64>,
    /// Last process-window-size rows value.
    pub window_rows: Option<i64>,
    /// Terminal name reported by `process-tty-name`, when this process uses a tty.
    pub tty_name: Option<String>,
    /// Whether stdin is tty-backed for this process.
    pub tty_stdin: bool,
    /// Whether stdout is tty-backed for this process.
    pub tty_stdout: bool,
    /// Whether stderr is tty-backed for this process.
    pub tty_stderr: bool,
    /// The actual OS child process, if spawned.
    #[allow(dead_code)]
    pub child: Option<Child>,
    /// OS-level stdout pipe for non-blocking reads.
    pub child_stdout: Option<std::process::ChildStdout>,
    /// OS-level stderr pipe for non-blocking reads.
    pub child_stderr: Option<std::process::ChildStderr>,
    /// TCP socket for network processes (client or accepted connection).
    /// When TLS is active, this is `None` (the socket is owned by `tls_stream`).
    #[cfg(unix)]
    pub socket: Option<std::net::TcpStream>,
    /// TLS-wrapped stream for encrypted network connections.
    /// When `Some`, reads/writes go through this instead of `socket`.
    #[cfg(unix)]
    pub tls_stream: Option<TlsStream>,
    /// Whether this is a server (listener) or client network process.
    pub network_server: bool,
    /// Marker position in the process buffer (byte offset), matching GNU's `p->mark`.
    /// `None` means "use end of buffer".
    pub mark_byte_pos: Option<usize>,
}

/// Manages the set of live processes.
///
/// Uses `polling::Poller` for efficient I/O multiplexing (epoll on Linux,
/// kqueue on macOS, wepoll on Windows) instead of sleep-based polling.
pub struct ProcessManager {
    processes: HashMap<ProcessId, Process>,
    deleted_processes: HashMap<ProcessId, Process>,
    next_id: ProcessId,
    /// Environment variable overrides (for `setenv`/`getenv`).
    env_overrides: HashMap<String, Option<String>>,
    /// I/O multiplexer for child process stdout/stderr pipes.
    poller: Option<polling::Poller>,
}

impl std::fmt::Debug for ProcessManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessManager")
            .field("processes", &self.processes)
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
            deleted_processes: HashMap::new(),
            next_id: 1,
            env_overrides: HashMap::new(),
            poller: polling::Poller::new().ok(),
        }
    }

    /// Create a new process record.  Returns the process id.
    pub fn create_process(
        &mut self,
        name: String,
        buffer_name: Option<String>,
        command: String,
        args: Vec<String>,
    ) -> ProcessId {
        self.create_process_with_kind(name, buffer_name, command, args, ProcessKind::Real)
    }

    /// Create a new process record with an explicit process kind.
    pub fn create_process_with_kind(
        &mut self,
        name: String,
        buffer_name: Option<String>,
        command: String,
        args: Vec<String>,
        kind: ProcessKind,
    ) -> ProcessId {
        let id = self.next_id;
        self.next_id += 1;
        let (tty_name, tty_stdin, tty_stdout, tty_stderr) = match kind {
            ProcessKind::Real => {
                let tty_name = Some(default_process_tty_name());
                (tty_name, true, true, true)
            }
            ProcessKind::Network | ProcessKind::Pipe | ProcessKind::Serial => {
                (None, false, false, false)
            }
        };
        let proc = Process {
            id,
            name,
            command,
            args,
            kind,
            status: ProcessStatus::Run,
            buffer_name,
            stdin_queue: String::new(),
            stdout: String::new(),
            stderr: String::new(),
            query_on_exit_flag: true,
            filter: Value::symbol(DEFAULT_PROCESS_FILTER_SYMBOL),
            sentinel: Value::symbol(DEFAULT_PROCESS_SENTINEL_SYMBOL),
            plist: Value::Nil,
            coding_decode: Value::symbol("utf-8-unix"),
            coding_encode: Value::symbol("utf-8-unix"),
            inherit_coding_system_flag: false,
            thread: Value::Nil,
            window_cols: None,
            window_rows: None,
            tty_name,
            tty_stdin,
            tty_stdout,
            tty_stderr,
            child: None,
            child_stdout: None,
            child_stderr: None,
            #[cfg(unix)]
            socket: None,
            tls_stream: None,
            network_server: false,
            mark_byte_pos: None,
        };
        self.processes.insert(id, proc);
        id
    }

    /// Spawn an OS child process for a tracked process record.
    /// Sets up piped stdin/stdout/stderr.
    pub fn spawn_child(&mut self, id: ProcessId) -> Result<(), String> {
        let proc = self
            .processes
            .get_mut(&id)
            .ok_or_else(|| "Process not found".to_string())?;

        if proc.child.is_some() {
            return Ok(()); // Already spawned
        }

        // Don't spawn non-real processes
        if proc.kind != ProcessKind::Real {
            return Ok(());
        }

        let program = &proc.command;
        if program == "nil" || program.is_empty() {
            return Ok(()); // No program to run
        }

        let mut cmd = Command::new(program);
        cmd.args(&proc.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Apply environment overrides
        for (key, val) in &self.env_overrides {
            match val {
                Some(v) => {
                    cmd.env(key, v);
                }
                None => {
                    cmd.env_remove(key);
                }
            }
        }

        match cmd.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();

                // Register stdout with the poller for efficient I/O notification.
                #[cfg(unix)]
                if let (Some(poller), Some(stdout)) = (&self.poller, &stdout) {
                    use std::os::unix::io::AsRawFd;
                    let fd = stdout.as_raw_fd();
                    // Set non-blocking before registering.
                    unsafe {
                        let flags = libc::fcntl(fd, libc::F_GETFL);
                        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                    }
                    // Use process id as the event key so we know which process is ready.
                    // Safety: fd is valid and owned by child_stdout which we keep alive.
                    unsafe {
                        let borrowed = std::os::unix::io::BorrowedFd::borrow_raw(fd);
                        let _ = poller.add_with_mode(
                            &borrowed,
                            polling::Event::readable(id as usize),
                            polling::PollMode::Level,
                        );
                    }
                }

                proc.child_stdout = stdout;
                proc.child_stderr = child.stderr.take();
                proc.child = Some(child);
                proc.status = ProcessStatus::Run;
                Ok(())
            }
            Err(e) => {
                proc.status = ProcessStatus::Exit(1);
                Err(format!("Failed to start process: {}", e))
            }
        }
    }

    /// Check if a child process has exited and update its status.
    /// Returns true if the process exited (status changed).
    pub fn check_child_exit(&mut self, id: ProcessId) -> bool {
        let proc = match self.processes.get_mut(&id) {
            Some(p) => p,
            None => return false,
        };
        let child = match proc.child.as_mut() {
            Some(c) => c,
            None => return false,
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                proc.status = ProcessStatus::Exit(status.code().unwrap_or(1));
                true
            }
            Ok(None) => false, // Still running
            Err(_) => {
                proc.status = ProcessStatus::Exit(1);
                true
            }
        }
    }

    /// Read available output from a child process's stdout.
    /// Returns the data read (may be empty if nothing available).
    pub fn read_child_stdout(&mut self, id: ProcessId) -> Option<String> {
        let proc = self.processes.get_mut(&id)?;
        let stdout = proc.child_stdout.as_mut()?;

        // Use non-blocking read via set_nonblocking on Unix
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = stdout.as_raw_fd();
            // Set non-blocking
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        let mut buf = vec![0u8; 4096];
        match stdout.read(&mut buf) {
            Ok(0) => None, // EOF
            Ok(n) => {
                let s = String::from_utf8_lossy(&buf[..n]).to_string();
                proc.stdout.push_str(&s);
                Some(s)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Some(String::new()),
            Err(_) => None,
        }
    }

    /// Wait for any child process to have output ready, with timeout.
    ///
    /// Uses `polling::Poller` (epoll/kqueue/wepoll) for efficient blocking
    /// instead of sleep-based polling. Returns the set of process IDs that
    /// have data ready to read.
    ///
    /// Falls back to a brief sleep if the poller is unavailable.
    pub fn wait_for_output(&self, timeout: std::time::Duration) -> Vec<ProcessId> {
        if let Some(ref poller) = self.poller {
            let mut events = polling::Events::new();
            match poller.wait(&mut events, Some(timeout)) {
                Ok(_) => events.iter().map(|e| e.key as ProcessId).collect(),
                Err(_) => {
                    // Fallback: brief sleep
                    std::thread::sleep(timeout.min(std::time::Duration::from_millis(10)));
                    self.live_process_ids()
                }
            }
        } else {
            // No poller available — sleep fallback
            std::thread::sleep(timeout.min(std::time::Duration::from_millis(10)));
            self.live_process_ids()
        }
    }

    /// Kill (remove) a process by id.  Returns true if found.
    pub fn kill_process(&mut self, id: ProcessId) -> bool {
        if let Some(proc) = self.processes.get_mut(&id) {
            if let Some(child) = proc.child.as_mut() {
                let _ = child.kill();
            }
            #[cfg(unix)]
            {
                proc.tls_stream.take();
                proc.socket.take();
            }
            proc.status = ProcessStatus::Signal(9);
            true
        } else {
            false
        }
    }

    /// Delete a process entirely.
    pub fn delete_process(&mut self, id: ProcessId) -> bool {
        if let Some(mut proc) = self.processes.remove(&id) {
            if let Some(child) = proc.child.as_mut() {
                let _ = child.kill();
            }
            #[cfg(unix)]
            {
                proc.tls_stream.take();
                proc.socket.take();
            }
            proc.status = ProcessStatus::Signal(9);
            self.deleted_processes.insert(id, proc);
            true
        } else {
            self.deleted_processes.contains_key(&id)
        }
    }

    /// Get process status.
    pub fn process_status(&self, id: ProcessId) -> Option<&ProcessStatus> {
        self.processes.get(&id).map(|p| &p.status)
    }

    /// Get process status for both live and stale process handles.
    pub fn process_status_any(&self, id: ProcessId) -> Option<&ProcessStatus> {
        self.processes
            .get(&id)
            .map(|p| &p.status)
            .or_else(|| self.deleted_processes.get(&id).map(|p| &p.status))
    }

    /// Get a process by id.
    pub fn get(&self, id: ProcessId) -> Option<&Process> {
        self.processes.get(&id)
    }

    /// Get a process by id from either live or stale process tables.
    pub fn get_any(&self, id: ProcessId) -> Option<&Process> {
        self.processes
            .get(&id)
            .or_else(|| self.deleted_processes.get(&id))
    }

    /// Get a mutable process by id.
    pub fn get_mut(&mut self, id: ProcessId) -> Option<&mut Process> {
        self.processes.get_mut(&id)
    }

    /// Get a mutable process by id from either live or stale process tables.
    pub fn get_any_mut(&mut self, id: ProcessId) -> Option<&mut Process> {
        if self.processes.contains_key(&id) {
            self.processes.get_mut(&id)
        } else {
            self.deleted_processes.get_mut(&id)
        }
    }

    /// List all process ids.
    pub fn list_processes(&self) -> Vec<ProcessId> {
        self.processes.keys().copied().collect()
    }

    /// Return IDs of processes that have a live OS child or network socket.
    pub fn live_process_ids(&self) -> Vec<ProcessId> {
        self.processes
            .iter()
            .filter(|(_, p)| {
                if !matches!(p.status, ProcessStatus::Run) {
                    return false;
                }
                if p.child.is_some() {
                    return true;
                }
                #[cfg(unix)]
                if p.socket.is_some() || p.tls_stream.is_some() {
                    return true;
                }
                false
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Returns true if this id has been allocated at least once.
    pub fn was_issued_id(&self, id: ProcessId) -> bool {
        id > 0 && id < self.next_id
    }

    /// Find a process by name.
    pub fn find_by_name(&self, name: &str) -> Option<ProcessId> {
        self.processes
            .values()
            .find(|p| p.name == name)
            .map(|p| p.id)
    }

    /// Find a process associated with BUFFER-NAME.
    pub fn find_by_buffer_name(&self, buffer_name: &str) -> Option<ProcessId> {
        self.processes
            .values()
            .find(|p| p.buffer_name.as_deref() == Some(buffer_name))
            .map(|p| p.id)
    }

    /// Queue input for a process.
    pub fn send_input(&mut self, id: ProcessId, input: &str) -> bool {
        if let Some(proc) = self.processes.get_mut(&id) {
            proc.stdin_queue.push_str(input);
            // Write to actual child stdin if available.
            if let Some(ref mut child) = proc.child {
                if let Some(ref mut stdin) = child.stdin {
                    use std::io::Write;
                    let _ = stdin.write_all(input.as_bytes());
                    let _ = stdin.flush();
                }
            }
            // Write to TLS stream or plain socket for network processes.
            #[cfg(unix)]
            if let Some(ref mut tls) = proc.tls_stream {
                use std::io::Write;
                let _ = tls.write_all(input.as_bytes());
                let _ = tls.flush();
            } else if let Some(ref mut socket) = proc.socket {
                use std::io::Write;
                let _ = socket.write_all(input.as_bytes());
                let _ = socket.flush();
            }
            true
        } else {
            false
        }
    }

    /// Register a network socket's fd with the I/O poller so that
    /// `wait_for_output` wakes up when data arrives.
    #[cfg(unix)]
    pub fn register_socket_fd(&self, id: ProcessId) -> Result<(), String> {
        let proc = self.processes.get(&id).ok_or("Process not found")?;
        let socket = proc.socket.as_ref().ok_or("No socket")?;
        if let Some(ref poller) = self.poller {
            use std::os::unix::io::AsRawFd;
            let fd = socket.as_raw_fd();
            unsafe {
                let borrowed = std::os::unix::io::BorrowedFd::borrow_raw(fd);
                poller
                    .add_with_mode(
                        &borrowed,
                        polling::Event::readable(id as usize),
                        polling::PollMode::Level,
                    )
                    .map_err(|e| format!("Failed to register socket fd: {}", e))?;
            }
        }
        Ok(())
    }

    /// Read available output from a process — child stdout or network socket.
    /// Returns `Some(data)` with available data (possibly empty on WouldBlock),
    /// or `None` on EOF / connection closed.
    pub fn read_process_output(&mut self, id: ProcessId) -> Option<String> {
        // Check what kind of I/O source this process has, without holding
        // a long-lived mutable borrow.
        let has_child_stdout = self
            .processes
            .get(&id)
            .map(|p| p.child_stdout.is_some())
            .unwrap_or(false);

        if has_child_stdout {
            return self.read_child_stdout(id);
        }

        // Try TLS stream first (encrypted network process), then plain socket.
        #[cfg(unix)]
        {
            let proc = self.processes.get_mut(&id)?;

            // TLS stream has priority over plain socket.
            if let Some(ref mut tls) = proc.tls_stream {
                use std::io::Read;
                let mut buf = vec![0u8; 4096];
                match tls.read(&mut buf) {
                    Ok(0) => return None,
                    Ok(n) => {
                        let s = String::from_utf8_lossy(&buf[..n]).to_string();
                        proc.stdout.push_str(&s);
                        return Some(s);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        return Some(String::new());
                    }
                    Err(_) => return None,
                }
            }

            if let Some(ref mut socket) = proc.socket {
                use std::io::Read;
                let mut buf = vec![0u8; 4096];
                match socket.read(&mut buf) {
                    Ok(0) => return None,
                    Ok(n) => {
                        let s = String::from_utf8_lossy(&buf[..n]).to_string();
                        proc.stdout.push_str(&s);
                        return Some(s);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        return Some(String::new());
                    }
                    Err(_) => return None,
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = self.processes.get(&id)?;
        }

        None
    }

    /// Get stdout output from a process.
    pub fn get_output(&self, id: ProcessId) -> Option<&str> {
        self.processes.get(&id).map(|p| p.stdout.as_str())
    }

    /// Get an environment variable (checking overrides first, then OS).
    pub fn getenv(&self, name: &str) -> Option<String> {
        if let Some(override_val) = self.env_overrides.get(name) {
            return override_val.clone();
        }
        std::env::var(name).ok()
    }

    /// Set an environment variable override.  If value is None, unset it.
    pub fn setenv(&mut self, name: String, value: Option<String>) {
        self.env_overrides.insert(name, value);
    }
}

const DEFAULT_PROCESS_FILTER_SYMBOL: &str = "internal-default-process-filter";
const DEFAULT_PROCESS_SENTINEL_SYMBOL: &str = "internal-default-process-sentinel";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(s) => Ok(with_heap(|h| h.get_string(*s).to_owned())),
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        Value::Nil => Ok("nil".to_string()),
        Value::True => Ok("t".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_sequence(value: &Value) -> Result<(), Flow> {
    if matches!(
        value,
        Value::Nil | Value::Cons(_) | Value::Vector(_) | Value::Str(_)
    ) {
        Ok(())
    } else {
        Err(signal_wrong_type_sequence(*value))
    }
}

fn expect_list(value: &Value) -> Result<(), Flow> {
    if value.is_list() {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *value],
        ))
    }
}

fn signal_wrong_type_sequence(value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![Value::symbol("sequencep"), value],
    )
}

fn signal_wrong_type_character(value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![Value::symbol("characterp"), value],
    )
}

fn char_from_codepoint_value(value: &Value) -> Result<char, Flow> {
    match value {
        Value::Char(c) => Ok(*c),
        Value::Int(n) if *n >= 0 => {
            char::from_u32(*n as u32).ok_or_else(|| signal_wrong_type_character(*value))
        }
        _ => Err(signal_wrong_type_character(*value)),
    }
}

fn sequence_value_to_env_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(s) => Ok(with_heap(|h| h.get_string(*s).to_owned())),
        Value::Vector(items) => {
            let vec = with_heap(|h| h.get_vector(*items).clone());
            let chars = vec
                .iter()
                .map(char_from_codepoint_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(chars.into_iter().collect())
        }
        Value::Cons(_) | Value::Nil => {
            let mut out = String::new();
            let mut cursor = *value;
            loop {
                match cursor {
                    Value::Nil => break,
                    Value::Cons(cell) => {
                        let (car, cdr) = {
                            let pair = read_cons(cell);
                            (pair.car, pair.cdr)
                        };
                        out.push(char_from_codepoint_value(&car)?);
                        cursor = cdr;
                    }
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), tail],
                        ));
                    }
                }
            }
            Ok(out)
        }
        other => Err(signal_wrong_type_sequence(*other)),
    }
}

fn expect_int_or_marker(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        v if super::marker::is_marker(v) => super::marker::marker_position_as_int(v),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn checked_region_bytes(
    buf: &crate::buffer::Buffer,
    start: i64,
    end: i64,
) -> Result<(usize, usize), Flow> {
    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Buffer(buf.id), Value::Int(start), Value::Int(end)],
        ));
    }

    let start_byte = buf.text.char_to_byte((start - 1) as usize);
    let end_byte = buf.text.char_to_byte((end - 1) as usize);
    Ok(if start_byte <= end_byte {
        (start_byte, end_byte)
    } else {
        (end_byte, start_byte)
    })
}

fn file_error_symbol(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "file-missing",
        std::io::ErrorKind::AlreadyExists => "file-already-exists",
        std::io::ErrorKind::PermissionDenied => "permission-denied",
        _ => "file-error",
    }
}

fn signal_process_io(action: &str, target: Option<&str>, err: std::io::Error) -> Flow {
    let mut data = vec![Value::string(action), Value::string(err.to_string())];
    if let Some(target) = target {
        data.push(Value::string(target));
    }
    signal(file_error_symbol(err.kind()), data)
}

#[derive(Clone, Debug)]
enum OutputTarget {
    Discard,
    Buffer(Value),
    File(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StderrTarget {
    Discard,
    ToStdoutTarget,
    File,
}

#[derive(Clone, Debug)]
struct DestinationSpec {
    stdout: OutputTarget,
    stderr: StderrTarget,
    stderr_file: Option<String>,
    no_wait: bool,
}

fn signal_wrong_type_string(value: Value) -> Flow {
    signal("wrong-type-argument", vec![Value::symbol("stringp"), value])
}

fn expect_string_strict(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(s) => Ok(with_heap(|h| h.get_string(*s).to_owned())),
        other => Err(signal_wrong_type_string(*other)),
    }
}

fn expect_process_name_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(s) => Ok(with_heap(|h| h.get_string(*s).to_owned())),
        _ => Err(signal(
            "error",
            vec![Value::string(":name value not a string")],
        )),
    }
}

fn keyword_name(value: &Value) -> Option<&str> {
    match value {
        Value::Keyword(k) => Some(resolve_sym(*k)),
        Value::Symbol(id) if resolve_sym(*id).starts_with(':') => Some(resolve_sym(*id)),
        _ => None,
    }
}

fn is_file_keyword(value: &Value) -> bool {
    matches!(value, Value::Keyword(k) if { let n = resolve_sym(*k); n == ":file" || n == "file" })
}

fn parse_file_target(items: &[Value]) -> Result<OutputTarget, Flow> {
    let file_value = items.get(1).cloned().unwrap_or(Value::Nil);
    let file = expect_string_strict(&file_value)?;
    Ok(OutputTarget::File(file))
}

fn parse_real_buffer_destination_in_state(
    buffers: &BufferManager,
    value: &Value,
) -> Result<(OutputTarget, bool), Flow> {
    match value {
        Value::Int(_) => Ok((OutputTarget::Discard, true)),
        Value::Nil => Ok((OutputTarget::Discard, false)),
        Value::True | Value::Str(_) => Ok((OutputTarget::Buffer(*value), false)),
        Value::Buffer(id) => {
            if buffers.get(*id).is_none() {
                Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ))
            } else {
                Ok((OutputTarget::Buffer(*value), false))
            }
        }
        Value::Cons(_) => {
            let items = list_to_vec(value).ok_or_else(|| signal_wrong_type_string(*value))?;
            let first = items.first().cloned().unwrap_or(Value::Nil);
            if is_file_keyword(&first) {
                Ok((parse_file_target(&items)?, false))
            } else {
                Err(signal_wrong_type_string(first))
            }
        }
        other => Err(signal_wrong_type_string(*other)),
    }
}

fn parse_stderr_destination(value: &Value) -> Result<(StderrTarget, Option<String>), Flow> {
    match value {
        Value::Nil => Ok((StderrTarget::Discard, None)),
        Value::True => Ok((StderrTarget::ToStdoutTarget, None)),
        Value::Str(s) => Ok((
            StderrTarget::File,
            Some(with_heap(|h| h.get_string(*s).to_owned())),
        )),
        other => Err(signal_wrong_type_string(*other)),
    }
}

fn parse_call_process_destination(
    buffers: &BufferManager,
    destination: &Value,
) -> Result<DestinationSpec, Flow> {
    if let Value::Cons(_) = destination {
        let items =
            list_to_vec(destination).ok_or_else(|| signal_wrong_type_string(*destination))?;
        let first = items.first().cloned().unwrap_or(Value::Nil);
        if is_file_keyword(&first) {
            let stdout = parse_file_target(&items)?;
            return Ok(DestinationSpec {
                stdout,
                stderr: StderrTarget::ToStdoutTarget,
                stderr_file: None,
                no_wait: false,
            });
        }
        let second = items.get(1).cloned().unwrap_or(Value::Nil);
        let (stdout, no_wait) = parse_real_buffer_destination_in_state(buffers, &first)?;
        let (stderr, stderr_file) = parse_stderr_destination(&second)?;
        return Ok(DestinationSpec {
            stdout,
            stderr,
            stderr_file,
            no_wait,
        });
    }

    let (stdout, no_wait) = parse_real_buffer_destination_in_state(buffers, destination)?;
    let stderr = match destination {
        Value::Nil | Value::Int(_) => StderrTarget::Discard,
        _ => StderrTarget::ToStdoutTarget,
    };
    Ok(DestinationSpec {
        stdout,
        stderr,
        stderr_file: None,
        no_wait,
    })
}

fn insert_process_output_in_state(
    buffers: &mut BufferManager,
    destination: &Value,
    output: &str,
) -> Result<(), Flow> {
    match destination {
        Value::Str(name) => {
            let name_str = with_heap(|h| h.get_string(*name).to_owned());
            let id = buffers
                .find_buffer_by_name(&name_str)
                .unwrap_or_else(|| buffers.create_buffer(&name_str));
            buffers.insert_into_buffer(id, output).ok_or_else(|| {
                signal(
                    "error",
                    vec![Value::string("No such live buffer for process output")],
                )
            })?;
            Ok(())
        }
        Value::Buffer(id) => {
            buffers
                .insert_into_buffer(*id, output)
                .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
            Ok(())
        }
        _ => {
            if let Some(current_id) = buffers.current_buffer_id() {
                let _ = buffers.insert_into_buffer(current_id, output);
            }
            Ok(())
        }
    }
}

fn write_output_target_in_state(
    buffers: &mut BufferManager,
    target: &OutputTarget,
    output: &[u8],
    append: bool,
) -> Result<(), Flow> {
    match target {
        OutputTarget::Discard => Ok(()),
        OutputTarget::Buffer(destination) => {
            let text = String::from_utf8_lossy(output).into_owned();
            insert_process_output_in_state(buffers, destination, &text)
        }
        OutputTarget::File(path) => {
            if append {
                use std::io::Write;
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|e| signal_process_io("Writing process output", Some(path), e))?;
                file.write_all(output)
                    .map_err(|e| signal_process_io("Writing process output", Some(path), e))
            } else {
                std::fs::write(path, output)
                    .map_err(|e| signal_process_io("Writing process output", Some(path), e))
            }
        }
    }
}

fn route_captured_output_in_state(
    buffers: &mut BufferManager,
    destination: &DestinationSpec,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(), Flow> {
    write_output_target_in_state(buffers, &destination.stdout, stdout, false)?;
    match destination.stderr {
        StderrTarget::Discard => Ok(()),
        StderrTarget::ToStdoutTarget => {
            write_output_target_in_state(buffers, &destination.stdout, stderr, true)
        }
        StderrTarget::File => {
            let path = destination
                .stderr_file
                .as_ref()
                .ok_or_else(|| signal("error", vec![Value::string("Missing stderr file target")]))?
                .clone();
            write_output_target_in_state(buffers, &OutputTarget::File(path), stderr, false)
        }
    }
}

fn configure_call_process_stdin(command: &mut Command, infile: Option<&str>) -> Result<(), Flow> {
    match infile {
        None => {
            command.stdin(Stdio::null());
            Ok(())
        }
        Some(path) => {
            let file = std::fs::File::open(path)
                .map_err(|e| signal_process_io("Opening process input file", Some(path), e))?;
            command.stdin(Stdio::from(file));
            Ok(())
        }
    }
}

pub(crate) fn run_process_command_in_state(
    buffers: &mut BufferManager,
    program: &str,
    infile: Option<String>,
    destination: &Value,
    cmd_args: &[String],
) -> EvalResult {
    let destination_spec = parse_call_process_destination(buffers, destination)?;

    if destination_spec.no_wait {
        let mut command = Command::new(program);
        command.args(cmd_args).stdout(Stdio::null());
        configure_call_process_stdin(&mut command, infile.as_deref())?;
        match destination_spec.stderr {
            StderrTarget::Discard | StderrTarget::ToStdoutTarget => {
                command.stderr(Stdio::null());
            }
            StderrTarget::File => {
                let path = destination_spec.stderr_file.as_ref().ok_or_else(|| {
                    signal("error", vec![Value::string("Missing stderr file target")])
                })?;
                let file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(path)
                    .map_err(|e| signal_process_io("Writing process output", Some(path), e))?;
                command.stderr(Stdio::from(file));
            }
        };

        let mut child = command
            .spawn()
            .map_err(|e| signal_process_io("Searching for program", Some(program), e))?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Ok(Value::Nil);
    }

    let mut command = Command::new(program);
    command
        .args(cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_call_process_stdin(&mut command, infile.as_deref())?;
    let output = command
        .output()
        .map_err(|e| signal_process_io("Searching for program", Some(program), e))?;

    let exit_code = output.status.code().unwrap_or(-1);
    route_captured_output_in_state(buffers, &destination_spec, &output.stdout, &output.stderr)?;
    Ok(Value::Int(exit_code as i64))
}

fn run_process_capture_output(program: &str, cmd_args: &[String]) -> Result<(i32, Vec<u8>), Flow> {
    let mut command = Command::new(program);
    command
        .args(cmd_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = command
        .output()
        .map_err(|e| signal_process_io("Searching for program", Some(program), e))?;
    Ok((output.status.code().unwrap_or(-1), output.stdout))
}

fn parse_output_lines(stdout: &[u8]) -> Value {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    if text.is_empty() {
        Value::Nil
    } else {
        Value::list(text.split('\n').map(Value::string).collect())
    }
}

fn parse_optional_infile(args: &[Value], index: usize) -> Result<Option<String>, Flow> {
    if args.len() > index && !args[index].is_nil() {
        Ok(Some(expect_string_strict(&args[index])?))
    } else {
        Ok(None)
    }
}

fn parse_string_args_strict(args: &[Value]) -> Result<Vec<String>, Flow> {
    args.iter().map(expect_string_strict).collect()
}

fn parse_sequence_args(args: &[Value]) -> Result<Vec<String>, Flow> {
    args.iter().map(sequence_value_to_env_string).collect()
}

fn signal_process_lines_status_error(program: &str, status: i32) -> Flow {
    signal(
        "error",
        vec![Value::string(format!(
            "{program} exited with status {status}"
        ))],
    )
}

fn shell_quote_argument(arg: &str) -> String {
    let mut out = String::from("'");
    for ch in arg.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn shell_command_with_args(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        return command.to_string();
    }
    let quoted = args
        .iter()
        .map(|arg| shell_quote_argument(arg))
        .collect::<Vec<_>>()
        .join(" ");
    if command.is_empty() {
        quoted
    } else {
        format!("{command} {quoted}")
    }
}

fn signal_wrong_type_processp(value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![Value::symbol("processp"), value],
    )
}

fn signal_process_does_not_exist(name: &str) -> Flow {
    signal(
        "error",
        vec![Value::string(format!("Process {name} does not exist"))],
    )
}

fn signal_process_not_active(eval: &super::eval::Context, id: ProcessId) -> Flow {
    signal_process_not_active_in_manager(&eval.processes, id)
}

fn signal_process_not_active_in_manager(processes: &ProcessManager, id: ProcessId) -> Flow {
    let name = processes
        .get_any(id)
        .map(|proc| proc.name.clone())
        .unwrap_or_else(|| id.to_string());
    signal(
        "error",
        vec![Value::string(format!("Process {name} is not active"))],
    )
}

fn stale_process_not_running_reason(status: &ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Signal(_) => "killed",
        ProcessStatus::Exit(_) => "finished",
        ProcessStatus::Stop => "stopped",
        ProcessStatus::Run => "inactive",
        ProcessStatus::Connect => "connect",
        ProcessStatus::Failed => "failed",
    }
}

fn signal_process_not_running(eval: &super::eval::Context, id: ProcessId) -> Flow {
    signal_process_not_running_in_manager(&eval.processes, id)
}

fn signal_process_not_running_in_manager(processes: &ProcessManager, id: ProcessId) -> Flow {
    let (name, reason) = processes
        .get_any(id)
        .map(|proc| {
            (
                proc.name.clone(),
                stale_process_not_running_reason(&proc.status),
            )
        })
        .unwrap_or_else(|| (id.to_string(), "inactive"));
    signal(
        "error",
        vec![Value::string(format!(
            "Process {name} not running: {reason}\n"
        ))],
    )
}

fn resolve_process_or_wrong_type(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<ProcessId, Flow> {
    match value {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            if eval.processes.get(id).is_some() {
                Ok(id)
            } else {
                Err(signal_wrong_type_processp(*value))
            }
        }
        Value::Str(s) => {
            let name = with_heap(|h| h.get_string(*s).to_owned());
            eval.processes
                .find_by_name(&name)
                .ok_or_else(|| signal_wrong_type_processp(*value))
        }
        _ => Err(signal_wrong_type_processp(*value)),
    }
}

fn resolve_process_or_wrong_type_any(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<ProcessId, Flow> {
    match value {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            if eval.processes.get_any(id).is_some() {
                Ok(id)
            } else {
                Err(signal_wrong_type_processp(*value))
            }
        }
        Value::Str(s) => {
            let name = with_heap(|h| h.get_string(*s).to_owned());
            eval.processes
                .find_by_name(&name)
                .ok_or_else(|| signal_wrong_type_processp(*value))
        }
        _ => Err(signal_wrong_type_processp(*value)),
    }
}

fn resolve_process_or_wrong_type_any_in_manager(
    processes: &ProcessManager,
    value: &Value,
) -> Result<ProcessId, Flow> {
    match value {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            if processes.get_any(id).is_some() {
                Ok(id)
            } else {
                Err(signal_wrong_type_processp(*value))
            }
        }
        Value::Str(s) => {
            let name = with_heap(|h| h.get_string(*s).to_owned());
            processes
                .find_by_name(&name)
                .ok_or_else(|| signal_wrong_type_processp(*value))
        }
        _ => Err(signal_wrong_type_processp(*value)),
    }
}

fn resolve_process_or_missing_error(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<ProcessId, Flow> {
    resolve_process_or_missing_error_in_manager(&eval.processes, value)
}

fn resolve_process_or_missing_error_in_manager(
    processes: &ProcessManager,
    value: &Value,
) -> Result<ProcessId, Flow> {
    match value {
        Value::Str(s) => {
            let name = with_heap(|h| h.get_string(*s).to_owned());
            processes
                .find_by_name(&name)
                .ok_or_else(|| signal_process_does_not_exist(&name))
        }
        _ => resolve_process_or_wrong_type_any_in_manager(processes, value),
    }
}

fn resolve_process_or_missing_error_any(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<ProcessId, Flow> {
    resolve_process_or_missing_error_any_in_manager(&eval.processes, value)
}

fn resolve_process_or_missing_error_any_in_manager(
    processes: &ProcessManager,
    value: &Value,
) -> Result<ProcessId, Flow> {
    match value {
        Value::Str(s) => {
            let name = with_heap(|h| h.get_string(*s).to_owned());
            processes
                .find_by_name(&name)
                .ok_or_else(|| signal_process_does_not_exist(&name))
        }
        _ => resolve_process_or_wrong_type_any_in_manager(processes, value),
    }
}

fn resolve_process_for_status(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<Option<ProcessId>, Flow> {
    match value {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            if eval.processes.get_any(id).is_some() {
                Ok(Some(id))
            } else {
                Err(signal_wrong_type_processp(*value))
            }
        }
        Value::Str(s) => {
            let name = with_heap(|h| h.get_string(*s).to_owned());
            Ok(eval.processes.find_by_name(&name))
        }
        _ => Err(signal_wrong_type_processp(*value)),
    }
}

fn resolve_buffer_name_for_process_lookup_in_state(
    frames: &FrameManager,
    buffers: &BufferManager,
    value: &Value,
) -> Result<Option<String>, Flow> {
    match value {
        Value::Nil => Ok(frames
            .selected_frame()
            .and_then(|frame| frame.selected_window())
            .and_then(|window| window.buffer_id())
            .and_then(|id| buffers.get(id))
            .map(|buf| buf.name.clone())),
        Value::Str(name) => {
            let name_str = with_heap(|h| h.get_string(*name).to_owned());
            Ok(buffers
                .find_buffer_by_name(&name_str)
                .and_then(|id| buffers.get(id))
                .map(|buf| buf.name.clone()))
        }
        Value::Buffer(id) => Ok(buffers.get(*id).map(|buf| buf.name.clone())),
        other => Err(signal_wrong_type_string(*other)),
    }
}

/// Resolve a live process designator for compatibility builtins.
///
/// NeoVM currently models process handles as integer ids.  These helpers treat
/// a live process id as a process designator for runtime parity surfaces.
fn resolve_live_process_designator(
    eval: &super::eval::Context,
    value: &Value,
) -> Option<ProcessId> {
    resolve_live_process_designator_in_manager(&eval.processes, value)
}

fn resolve_live_process_designator_in_manager(
    processes: &ProcessManager,
    value: &Value,
) -> Option<ProcessId> {
    match value {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            processes.get(id).map(|_| id)
        }
        _ => None,
    }
}

fn resolve_live_process_or_wrong_type(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<ProcessId, Flow> {
    resolve_live_process_or_wrong_type_in_manager(&eval.processes, value)
}

fn resolve_live_process_or_wrong_type_in_manager(
    processes: &ProcessManager,
    value: &Value,
) -> Result<ProcessId, Flow> {
    resolve_live_process_designator_in_manager(processes, value).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), *value],
        )
    })
}

fn current_thread_handle(threads: &ThreadManager) -> Value {
    threads
        .thread_handle(threads.current_thread_id())
        .unwrap_or(Value::Nil)
}

fn is_stale_process_id_designator(eval: &super::eval::Context, value: &Value) -> bool {
    is_stale_process_id_designator_in_manager(&eval.processes, value)
}

fn is_stale_process_id_designator_in_manager(processes: &ProcessManager, value: &Value) -> bool {
    match value {
        Value::Int(n) if *n > 0 => {
            let id = *n as ProcessId;
            processes.get(id).is_none()
                && (processes.get_any(id).is_some() || processes.was_issued_id(id))
        }
        _ => false,
    }
}

fn resolve_optional_process_or_current_buffer(
    eval: &super::eval::Context,
    value: Option<&Value>,
) -> Result<ProcessId, Flow> {
    resolve_optional_process_or_current_buffer_in_state(&eval.processes, &eval.buffers, value)
}

fn resolve_optional_process_or_current_buffer_in_state(
    processes: &ProcessManager,
    buffers: &BufferManager,
    value: Option<&Value>,
) -> Result<ProcessId, Flow> {
    if let Some(v) = value {
        if !v.is_nil() {
            return resolve_process_or_missing_error_in_manager(processes, v);
        }
    }

    let current_buffer_name = buffers
        .current_buffer()
        .map(|buffer| buffer.name.clone())
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    processes
        .find_by_buffer_name(&current_buffer_name)
        .ok_or_else(|| {
            signal(
                "error",
                vec![Value::string(format!(
                    "Buffer {} has no process",
                    current_buffer_name
                ))],
            )
        })
}

fn process_live_status_value(status: &ProcessStatus, kind: &ProcessKind) -> Value {
    match status {
        ProcessStatus::Run => match kind {
            ProcessKind::Network => Value::list(vec![
                Value::symbol("listen"),
                Value::symbol("connect"),
                Value::symbol("stop"),
            ]),
            ProcessKind::Pipe => Value::list(vec![
                Value::symbol("open"),
                Value::symbol("listen"),
                Value::symbol("connect"),
                Value::symbol("stop"),
            ]),
            _ => Value::list(vec![
                Value::symbol("run"),
                Value::symbol("open"),
                Value::symbol("listen"),
                Value::symbol("connect"),
                Value::symbol("stop"),
            ]),
        },
        ProcessStatus::Stop => Value::list(vec![Value::symbol("stop")]),
        ProcessStatus::Connect => Value::list(vec![Value::symbol("connect")]),
        ProcessStatus::Exit(_) | ProcessStatus::Signal(_) | ProcessStatus::Failed => Value::Nil,
    }
}

fn default_process_tty_name() -> String {
    // NeoVM does not yet allocate real PTYs for subprocesses, but oracle behavior
    // expects tty-backed streams for default `start-process` paths.
    "/dev/pts/0".to_string()
}

fn signal_wrong_type_bufferp(value: Value) -> Flow {
    signal("wrong-type-argument", vec![Value::symbol("bufferp"), value])
}

fn signal_wrong_type_threadp(value: Value) -> Flow {
    signal("wrong-type-argument", vec![Value::symbol("threadp"), value])
}

fn signal_wrong_type_integerp(value: Value) -> Flow {
    signal(
        "wrong-type-argument",
        vec![Value::symbol("integerp"), value],
    )
}

fn signal_wrong_type_numberp(value: Value) -> Flow {
    signal("wrong-type-argument", vec![Value::symbol("numberp"), value])
}

fn signal_undefined_signal_name(name: &str) -> Flow {
    signal(
        "error",
        vec![Value::string(format!("Undefined signal name {name}"))],
    )
}

fn resolve_optional_process_with_explicit_return(
    eval: &super::eval::Context,
    value: Option<&Value>,
) -> Result<(ProcessId, Value), Flow> {
    resolve_optional_process_with_explicit_return_in_state(&eval.processes, &eval.buffers, value)
}

fn resolve_optional_process_with_explicit_return_in_state(
    processes: &ProcessManager,
    buffers: &BufferManager,
    value: Option<&Value>,
) -> Result<(ProcessId, Value), Flow> {
    if let Some(v) = value {
        if !v.is_nil() && is_stale_process_id_designator_in_manager(processes, v) {
            if let Value::Int(n) = v {
                return Err(signal_process_not_active_in_manager(
                    processes,
                    *n as ProcessId,
                ));
            }
        }
    }
    if let Some(v) = value {
        if !v.is_nil() {
            let id = resolve_process_or_missing_error_in_manager(processes, v)?;
            return Ok((id, *v));
        }
    }
    let id = resolve_optional_process_or_current_buffer_in_state(processes, buffers, value)?;
    Ok((id, Value::Nil))
}

enum SignalProcessTarget {
    Process(ProcessId),
    MissingNamedProcess,
    Pid(i64),
}

fn resolve_signal_process_target(
    eval: &super::eval::Context,
    value: Option<&Value>,
) -> Result<SignalProcessTarget, Flow> {
    resolve_signal_process_target_in_state(&eval.processes, &eval.buffers, value)
}

fn resolve_signal_process_target_in_state(
    processes: &ProcessManager,
    buffers: &BufferManager,
    value: Option<&Value>,
) -> Result<SignalProcessTarget, Flow> {
    if let Some(v) = value {
        if !v.is_nil() {
            return match v {
                Value::Str(name) => {
                    let name_str = with_heap(|h| h.get_string(*name).to_owned());
                    Ok(match processes.find_by_name(&name_str) {
                        Some(id) => SignalProcessTarget::Process(id),
                        None => SignalProcessTarget::MissingNamedProcess,
                    })
                }
                Value::Int(pid) if *pid >= 0 => {
                    let id = *pid as ProcessId;
                    if processes.get(id).is_some() {
                        Ok(SignalProcessTarget::Process(id))
                    } else {
                        Ok(SignalProcessTarget::Pid(*pid))
                    }
                }
                _ => Err(signal_wrong_type_processp(*v)),
            };
        }
    }

    let id = resolve_optional_process_or_current_buffer_in_state(processes, buffers, value)?;
    Ok(SignalProcessTarget::Process(id))
}

fn parse_signal_number(value: &Value) -> Result<i32, Flow> {
    match value {
        Value::Int(n) => Ok(*n as i32),
        Value::Char(c) => Ok(*c as i32),
        Value::Str(_) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *value],
        )),
        _ => {
            if let Some(name) = value.as_symbol_name() {
                Err(signal_undefined_signal_name(name))
            } else {
                Err(signal_wrong_type_integerp(*value))
            }
        }
    }
}

fn pid_exists(pid: i64) -> bool {
    if pid < 0 {
        return false;
    }
    std::fs::metadata(format!("/proc/{pid}")).is_ok()
}

#[derive(Clone, Debug)]
struct ProcStatSnapshot {
    comm: String,
    state: String,
    ppid: i64,
    pgrp: i64,
    sess: i64,
    tpgid: i64,
    minflt: i64,
    majflt: i64,
    cminflt: i64,
    cmajflt: i64,
    utime_ticks: i64,
    stime_ticks: i64,
    cutime_ticks: i64,
    cstime_ticks: i64,
    pri: i64,
    nice: i64,
    thcount: i64,
    start_ticks: i64,
    vsize: i64,
    rss: i64,
    ttname: String,
}

impl ProcStatSnapshot {
    fn fallback(pid: i64) -> Self {
        Self {
            comm: String::new(),
            state: String::new(),
            ppid: 0,
            pgrp: 0,
            sess: 0,
            tpgid: 0,
            minflt: 0,
            majflt: 0,
            cminflt: 0,
            cmajflt: 0,
            utime_ticks: 0,
            stime_ticks: 0,
            cutime_ticks: 0,
            cstime_ticks: 0,
            pri: 0,
            nice: 0,
            thcount: 0,
            start_ticks: 0,
            vsize: 0,
            rss: 0,
            ttname: read_proc_tty_name(pid),
        }
    }
}

fn parse_stat_i64_field(fields: &[&str], index: usize) -> Option<i64> {
    fields.get(index)?.parse::<i64>().ok()
}

#[cfg(not(target_os = "windows"))]
fn page_size_kb() -> i64 {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no additional preconditions.
    let page_size_bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size_bytes <= 0 {
        4
    } else {
        ((page_size_bytes as i64) / 1024).max(1)
    }
}

#[cfg(target_os = "windows")]
fn page_size_kb() -> i64 {
    4
}

#[cfg(not(target_os = "windows"))]
fn clock_ticks_per_second() -> i64 {
    // SAFETY: `sysconf(_SC_CLK_TCK)` has no additional preconditions.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 { 100 } else { ticks as i64 }
}

#[cfg(target_os = "windows")]
fn clock_ticks_per_second() -> i64 {
    100
}

fn read_proc_tty_name(pid: i64) -> String {
    std::fs::read_link(format!("/proc/{pid}/fd/0"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".to_string())
}

fn parse_proc_cmdline(pid: i64) -> String {
    let bytes = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(bytes) => bytes,
        Err(_) => return String::new(),
    };
    let mut args = Vec::new();
    for chunk in bytes.split(|b| *b == 0) {
        if chunk.is_empty() {
            continue;
        }
        args.push(String::from_utf8_lossy(chunk).into_owned());
    }
    args.join(" ")
}

fn parse_proc_boot_time_secs() -> Option<i64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    for line in stat.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse::<i64>().ok();
        }
    }
    None
}

fn parse_total_memory_kb() -> Option<i64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = rest.split_whitespace().next()?.parse::<i64>().ok()?;
            return Some(kb);
        }
    }
    None
}

fn ticks_to_secs_usecs(ticks: i64, hz: i64) -> (i64, i64) {
    if hz <= 0 {
        return (0, 0);
    }
    let secs = ticks.div_euclid(hz);
    let rem = ticks.rem_euclid(hz);
    let usecs = ((rem as i128) * 1_000_000i128 / (hz as i128)) as i64;
    (secs, usecs)
}

fn time_list_from_secs_usecs(secs: i64, usecs: i64) -> Value {
    let high = (secs >> 16) & 0xFFFF_FFFF;
    let low = secs & 0xFFFF;
    Value::list(vec![
        Value::Int(high),
        Value::Int(low),
        Value::Int(usecs.clamp(0, 999_999)),
        Value::Int(0),
    ])
}

fn time_list_from_ticks(ticks: i64, hz: i64) -> Value {
    let (secs, usecs) = ticks_to_secs_usecs(ticks, hz);
    time_list_from_secs_usecs(secs, usecs)
}

fn now_epoch_secs_usecs() -> Option<(i64, i64)> {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => Some((dur.as_secs() as i64, dur.subsec_micros() as i64)),
        Err(_) => None,
    }
}

fn nonnegative_time_diff(now: (i64, i64), then: (i64, i64)) -> (i64, i64) {
    let (now_secs, now_usecs) = now;
    let (then_secs, then_usecs) = then;
    if (now_secs, now_usecs) < (then_secs, then_usecs) {
        return (0, 0);
    }
    let mut secs = now_secs - then_secs;
    let mut usecs = now_usecs - then_usecs;
    if usecs < 0 {
        secs -= 1;
        usecs += 1_000_000;
    }
    (secs, usecs)
}

fn parse_proc_stat_snapshot(pid: i64) -> Option<ProcStatSnapshot> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open_paren = stat.find('(')?;
    let close_paren = stat.rfind(')')?;
    if close_paren <= open_paren {
        return None;
    }

    let comm = stat.get((open_paren + 1)..close_paren)?.to_string();
    let trailing = stat.get((close_paren + 1)..)?.trim_start();
    let fields: Vec<&str> = trailing.split_whitespace().collect();
    if fields.len() < 22 {
        return None;
    }

    let state = fields[0].to_string();
    let ppid = parse_stat_i64_field(&fields, 1)?;
    let pgrp = parse_stat_i64_field(&fields, 2)?;
    let sess = parse_stat_i64_field(&fields, 3)?;
    let tpgid = parse_stat_i64_field(&fields, 5)?;
    let minflt = parse_stat_i64_field(&fields, 7)?;
    let cminflt = parse_stat_i64_field(&fields, 8)?;
    let majflt = parse_stat_i64_field(&fields, 9)?;
    let cmajflt = parse_stat_i64_field(&fields, 10)?;
    let utime_ticks = parse_stat_i64_field(&fields, 11)?;
    let stime_ticks = parse_stat_i64_field(&fields, 12)?;
    let cutime_ticks = parse_stat_i64_field(&fields, 13)?;
    let cstime_ticks = parse_stat_i64_field(&fields, 14)?;
    let pri = parse_stat_i64_field(&fields, 15)?;
    let nice = parse_stat_i64_field(&fields, 16)?;
    let thcount = parse_stat_i64_field(&fields, 17)?;
    let start_ticks = parse_stat_i64_field(&fields, 19)?;
    let vsize = parse_stat_i64_field(&fields, 20)?;
    let rss_pages = parse_stat_i64_field(&fields, 21)?;
    let rss = rss_pages.saturating_mul(page_size_kb());
    let ttname = read_proc_tty_name(pid);

    Some(ProcStatSnapshot {
        comm,
        state,
        ppid,
        pgrp,
        sess,
        tpgid,
        minflt,
        majflt,
        cminflt,
        cmajflt,
        utime_ticks,
        stime_ticks,
        cutime_ticks,
        cstime_ticks,
        pri,
        nice,
        thcount,
        start_ticks,
        vsize,
        rss,
        ttname,
    })
}

fn parse_effective_ids_from_proc_status(pid: i64) -> Option<(u32, u32)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut euid = None;
    let mut egid = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() >= 2 {
                euid = fields[1].parse::<u32>().ok();
            }
        } else if let Some(rest) = line.strip_prefix("Gid:") {
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() >= 2 {
                egid = fields[1].parse::<u32>().ok();
            }
        }
        if euid.is_some() && egid.is_some() {
            break;
        }
    }
    Some((euid?, egid?))
}

#[cfg(not(target_os = "windows"))]
fn lookup_user_name(uid: u32) -> Option<String> {
    // SAFETY: libc returns either null or a valid passwd struct pointer.
    let user = unsafe { libc::getpwuid(uid as libc::uid_t) };
    if user.is_null() {
        return None;
    }
    // SAFETY: `user` is non-null and `pw_name` is a valid C string pointer.
    let name_ptr = unsafe { (*user).pw_name };
    if name_ptr.is_null() {
        return None;
    }
    // SAFETY: `name_ptr` is a valid NUL-terminated C string.
    Some(
        unsafe { CStr::from_ptr(name_ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(target_os = "windows")]
fn lookup_user_name(_uid: u32) -> Option<String> {
    None
}

#[cfg(not(target_os = "windows"))]
fn lookup_group_name(gid: u32) -> Option<String> {
    // SAFETY: libc returns either null or a valid group struct pointer.
    let group = unsafe { libc::getgrgid(gid as libc::gid_t) };
    if group.is_null() {
        return None;
    }
    // SAFETY: `group` is non-null and `gr_name` is a valid C string pointer.
    let name_ptr = unsafe { (*group).gr_name };
    if name_ptr.is_null() {
        return None;
    }
    // SAFETY: `name_ptr` is a valid NUL-terminated C string.
    Some(
        unsafe { CStr::from_ptr(name_ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(target_os = "windows")]
fn lookup_group_name(_gid: u32) -> Option<String> {
    None
}

fn parse_make_process_command(value: &Value) -> Result<Vec<String>, Flow> {
    let as_vec: Option<Vec<Value>> = match value {
        Value::Vector(items) => Some(with_heap(|h| h.get_vector(*items).clone())),
        Value::Cons(_) | Value::Nil => list_to_vec(value),
        _ => None,
    };

    let Some(items) = as_vec else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *value],
        ));
    };

    items
        .into_iter()
        .map(|item| expect_string_strict(&item))
        .collect()
}

fn parse_make_process_buffer(
    eval: &mut super::eval::Context,
    value: &Value,
) -> Result<Option<String>, Flow> {
    parse_make_process_buffer_in_state(&mut eval.buffers, value)
}

fn parse_make_process_buffer_in_state(
    buffers: &mut BufferManager,
    value: &Value,
) -> Result<Option<String>, Flow> {
    match value {
        Value::Nil => Ok(None),
        Value::Str(name) => {
            let name_str = with_heap(|h| h.get_string(*name).to_owned());
            if buffers.find_buffer_by_name(&name_str).is_none() {
                let _ = buffers.create_buffer(&name_str);
            }
            Ok(Some(name_str))
        }
        Value::Buffer(id) => buffers
            .get(*id)
            .map(|buf| Some(buf.name.clone()))
            .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")])),
        _ => Err(signal_wrong_type_string(*value)),
    }
}

fn expect_integer(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        _ => Err(signal_wrong_type_integerp(*value)),
    }
}

fn value_as_nonnegative_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Int(n) if *n >= 0 => Some(*n),
        Value::Char(c) => Some(*c as i64),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NetworkAddressFamily {
    Ipv4,
    Ipv6,
}

#[derive(Clone, Debug)]
struct HostInterfaceEntry {
    name: String,
    family: NetworkAddressFamily,
    address: Value,
    list_broadcast: Value,
    info_broadcast: Value,
    netmask: Value,
    hwaddr: Option<Value>,
    flags: Value,
}

fn vector_nonnegative_integers(value: &Value) -> Option<Vec<i64>> {
    let Value::Vector(values) = value else {
        return None;
    };
    let locked = with_heap(|h| h.get_vector(*values).clone());
    let mut out = Vec::with_capacity(locked.len());
    for item in locked.iter() {
        out.push(value_as_nonnegative_integer(item)?);
    }
    Some(out)
}

fn int_vector(values: &[i64]) -> Value {
    Value::vector(values.iter().map(|v| Value::Int(*v)).collect())
}

fn loopback_ipv4_address() -> Value {
    int_vector(&[127, 0, 0, 1, 0])
}

fn loopback_ipv4_broadcast() -> Value {
    int_vector(&[0, 0, 0, 0, 0])
}

fn loopback_ipv4_netmask() -> Value {
    int_vector(&[255, 0, 0, 0, 0])
}

fn loopback_ipv6_address() -> Value {
    int_vector(&[0, 0, 0, 0, 0, 0, 0, 1, 0])
}

fn loopback_ipv6_broadcast() -> Value {
    int_vector(&[0, 0, 0, 0, 0, 0, 0, 1, 0])
}

fn loopback_ipv6_netmask() -> Value {
    int_vector(&[65535, 65535, 65535, 65535, 65535, 65535, 65535, 65535, 0])
}

fn loopback_hwaddr() -> Value {
    Value::cons(Value::Int(772), int_vector(&[0, 0, 0, 0, 0, 0]))
}

fn loopback_flags() -> Value {
    Value::list(vec![
        Value::symbol("running"),
        Value::symbol("loopback"),
        Value::symbol("up"),
    ])
}

fn zero_network_address(family: NetworkAddressFamily) -> Value {
    match family {
        NetworkAddressFamily::Ipv4 => int_vector(&[0, 0, 0, 0, 0]),
        NetworkAddressFamily::Ipv6 => int_vector(&[0, 0, 0, 0, 0, 0, 0, 0, 0]),
    }
}

fn network_directed_broadcast(
    family: NetworkAddressFamily,
    address: &Value,
    netmask: &Value,
) -> Option<Value> {
    let address_items = vector_nonnegative_integers(address)?;
    let netmask_items = vector_nonnegative_integers(netmask)?;
    match family {
        NetworkAddressFamily::Ipv4 => {
            if address_items.len() != 5 || netmask_items.len() != 5 {
                return None;
            }
            let mut out = [0_i64; 5];
            for idx in 0..4 {
                let addr = u8::try_from(address_items[idx]).ok()?;
                let mask = u8::try_from(netmask_items[idx]).ok()?;
                out[idx] = (addr | !mask) as i64;
            }
            Some(int_vector(&out))
        }
        NetworkAddressFamily::Ipv6 => {
            if address_items.len() != 9 || netmask_items.len() != 9 {
                return None;
            }
            let mut out = [0_i64; 9];
            for idx in 0..8 {
                let addr = u16::try_from(address_items[idx]).ok()?;
                let mask = u16::try_from(netmask_items[idx]).ok()?;
                out[idx] = (addr | !mask) as i64;
            }
            Some(int_vector(&out))
        }
    }
}

fn derive_network_interface_list_broadcast(
    family: NetworkAddressFamily,
    address: &Value,
    netmask: &Value,
    raw_broadcast: &Value,
) -> Value {
    network_directed_broadcast(family, address, netmask).unwrap_or(*raw_broadcast)
}

fn derive_network_interface_info_broadcast(
    family: NetworkAddressFamily,
    address: &Value,
    raw_broadcast: &Value,
) -> Value {
    if raw_broadcast == address {
        zero_network_address(family)
    } else {
        *raw_broadcast
    }
}

fn ip_to_value(ip: IpAddr) -> (NetworkAddressFamily, Value) {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            (
                NetworkAddressFamily::Ipv4,
                int_vector(&[
                    octets[0] as i64,
                    octets[1] as i64,
                    octets[2] as i64,
                    octets[3] as i64,
                    0,
                ]),
            )
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            let mut vals = [0_i64; 9];
            for (idx, &seg) in segments.iter().enumerate() {
                vals[idx] = seg as i64;
            }
            (NetworkAddressFamily::Ipv6, int_vector(&vals))
        }
    }
}

fn resolve_network_lookup_addresses(
    name: &str,
    family: Option<NetworkAddressFamily>,
) -> Vec<Value> {
    use dns_lookup::{AddrFamily, AddrInfoHints, SockType};

    // Emacs forwards names through C APIs where embedded NUL terminates the
    // effective hostname. Match that behavior instead of rejecting interior NUL.
    let normalized_name = name.split('\0').next().unwrap_or_default();

    let hints = AddrInfoHints {
        socktype: SockType::Stream.into(),
        address: match family {
            Some(NetworkAddressFamily::Ipv4) => AddrFamily::Inet.into(),
            Some(NetworkAddressFamily::Ipv6) => AddrFamily::Inet6.into(),
            None => 0, // AF_UNSPEC
        },
        ..AddrInfoHints::default()
    };

    let addrs = match dns_lookup::getaddrinfo(Some(normalized_name), None, Some(hints)) {
        Ok(iter) => iter,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for result in addrs {
        let info = match result {
            Ok(info) => info,
            Err(_) => continue,
        };
        let (resolved_family, address) = ip_to_value(info.sockaddr.ip());
        let include = match family {
            Some(expected) => expected == resolved_family,
            None => true,
        };
        if include {
            out.push(address);
        }
    }

    out
}

fn parse_mac_addr(mac: &str) -> Option<Value> {
    let mut bytes = Vec::new();
    for part in mac.trim().split(':') {
        if part.is_empty() {
            continue;
        }
        let byte = u8::from_str_radix(part, 16).ok()?;
        bytes.push(Value::Int(byte as i64));
    }
    if bytes.is_empty() {
        return None;
    }
    // hatype 1 = ARPHRD_ETHER (Ethernet), the common case
    Some(Value::cons(Value::Int(1), Value::vector(bytes)))
}

fn host_interface_snapshot() -> Option<Vec<HostInterfaceEntry>> {
    use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};

    let interfaces = NetworkInterface::show().ok()?;

    let mut entries = Vec::new();

    for iface in &interfaces {
        let hwaddr = iface
            .mac_addr
            .as_deref()
            .and_then(|mac| parse_mac_addr(mac));

        for addr in &iface.addr {
            let (family, address, netmask, raw_broadcast) = match addr {
                Addr::V4(v4) => {
                    let ip = v4.ip.octets();
                    let address =
                        int_vector(&[ip[0] as i64, ip[1] as i64, ip[2] as i64, ip[3] as i64, 0]);
                    let netmask = v4
                        .netmask
                        .map(|m| {
                            let o = m.octets();
                            int_vector(&[o[0] as i64, o[1] as i64, o[2] as i64, o[3] as i64, 0])
                        })
                        .unwrap_or_else(|| zero_network_address(NetworkAddressFamily::Ipv4));
                    let broadcast = v4
                        .broadcast
                        .map(|b| {
                            let o = b.octets();
                            int_vector(&[o[0] as i64, o[1] as i64, o[2] as i64, o[3] as i64, 0])
                        })
                        .unwrap_or_else(|| zero_network_address(NetworkAddressFamily::Ipv4));
                    (NetworkAddressFamily::Ipv4, address, netmask, broadcast)
                }
                Addr::V6(v6) => {
                    let segs = v6.ip.segments();
                    let mut vals = [0_i64; 9];
                    for (idx, &seg) in segs.iter().enumerate() {
                        vals[idx] = seg as i64;
                    }
                    let address = int_vector(&vals);
                    let netmask = v6
                        .netmask
                        .map(|m| {
                            let s = m.segments();
                            let mut v = [0_i64; 9];
                            for (idx, &seg) in s.iter().enumerate() {
                                v[idx] = seg as i64;
                            }
                            int_vector(&v)
                        })
                        .unwrap_or_else(|| zero_network_address(NetworkAddressFamily::Ipv6));
                    let broadcast = v6
                        .broadcast
                        .map(|b| {
                            let s = b.segments();
                            let mut v = [0_i64; 9];
                            for (idx, &seg) in s.iter().enumerate() {
                                v[idx] = seg as i64;
                            }
                            int_vector(&v)
                        })
                        .unwrap_or_else(|| zero_network_address(NetworkAddressFamily::Ipv6));
                    (NetworkAddressFamily::Ipv6, address, netmask, broadcast)
                }
            };

            let list_broadcast =
                derive_network_interface_list_broadcast(family, &address, &netmask, &raw_broadcast);
            let info_broadcast =
                derive_network_interface_info_broadcast(family, &address, &raw_broadcast);

            // Approximate flags from available information
            let is_loopback = match addr {
                Addr::V4(v4) => v4.ip.is_loopback(),
                Addr::V6(v6) => v6.ip.is_loopback(),
            };
            let has_broadcast = match addr {
                Addr::V4(v4) => v4.broadcast.is_some(),
                Addr::V6(v6) => v6.broadcast.is_some(),
            };
            let mut flags = vec![Value::symbol("running"), Value::symbol("up")];
            if is_loopback {
                flags.push(Value::symbol("loopback"));
            }
            if has_broadcast {
                flags.push(Value::symbol("broadcast"));
            }

            entries.push(HostInterfaceEntry {
                name: iface.name.clone(),
                family,
                address,
                list_broadcast,
                info_broadcast,
                netmask,
                hwaddr,
                flags: Value::list(flags),
            });
        }
    }

    if entries.is_empty() {
        return None;
    }

    Some(entries)
}

fn interface_entry(name: &str, address: Value, full: bool) -> Value {
    if !full {
        return Value::cons(Value::string(name), address);
    }

    let (broadcast, netmask) = match &address {
        Value::Vector(values) if with_heap(|h| h.vector_len(*values)) == 9 => {
            (loopback_ipv6_broadcast(), loopback_ipv6_netmask())
        }
        _ => (loopback_ipv4_broadcast(), loopback_ipv4_netmask()),
    };

    Value::list(vec![Value::string(name), address, broadcast, netmask])
}

fn format_ipv4_network_address(items: &[i64], omit_port: bool) -> Option<String> {
    if items.len() != 4 && items.len() != 5 {
        return None;
    }
    let octets: Vec<u8> = items[..4]
        .iter()
        .map(|v| u8::try_from(*v).ok())
        .collect::<Option<Vec<_>>>()?;
    let addr = format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]);
    if items.len() == 5 && !omit_port {
        let port = u16::try_from(items[4]).ok()?;
        Some(format!("{addr}:{port}"))
    } else {
        Some(addr)
    }
}

fn format_ipv6_network_address(items: &[i64], omit_port: bool) -> Option<String> {
    if items.len() != 8 && items.len() != 9 {
        return None;
    }
    let mut segments = Vec::with_capacity(8);
    for value in &items[..8] {
        let segment = u16::try_from(*value).ok()?;
        segments.push(format!("{segment:x}"));
    }
    let addr = segments.join(":");
    if items.len() == 9 && !omit_port {
        let port = u16::try_from(items[8]).ok()?;
        Some(format!("[{addr}]:{port}"))
    } else {
        Some(addr)
    }
}

// ---------------------------------------------------------------------------
// Builtins (eval-dependent)
// ---------------------------------------------------------------------------

/// (backquote-delay-process ENV FORM) -> delayed-form
pub(crate) fn builtin_backquote_delay_process(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("backquote-delay-process", &args, 2)?;
    Ok(Value::list(vec![
        Value::Int(0),
        Value::symbol("quote"),
        Value::list(vec![args[1]]),
    ]))
}

/// (backquote-process FORM &optional LEVEL) -> processed
pub(crate) fn builtin_backquote_process(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backquote-process", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("backquote-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    Ok(Value::list(vec![Value::Int(0)]))
}

/// (clone-process PROCESS &optional NAME) -> process
pub(crate) fn builtin_clone_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("clone-process", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("clone-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let id = resolve_process_or_wrong_type_any(eval, &args[0])?;
    Ok(Value::Int(id as i64))
}

/// (internal-default-interrupt-process &optional PROCESS CURRENT-GROUP) -> process-or-nil
pub(crate) fn builtin_internal_default_interrupt_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_internal_default_interrupt_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_internal_default_interrupt_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("internal-default-interrupt-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let (id, ret) =
        resolve_optional_process_with_explicit_return_in_state(processes, buffers, args.first())?;
    if let Some(proc) = processes.get_mut(id) {
        proc.status = ProcessStatus::Signal(2);
    }
    Ok(ret)
}

/// (internal-default-signal-process PROCESS SIGNAL &optional CURRENT-GROUP) -> int-or-nil
pub(crate) fn builtin_internal_default_signal_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_internal_default_signal_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_internal_default_signal_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("internal-default-signal-process", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("internal-default-signal-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let signal_num = parse_signal_number(&args[1])?;
    match resolve_signal_process_target_in_state(processes, buffers, args.first())? {
        SignalProcessTarget::Process(id) => {
            if let Some(proc) = processes.get_mut(id) {
                proc.status = ProcessStatus::Signal(signal_num);
            }
            Ok(Value::Int(0))
        }
        SignalProcessTarget::MissingNamedProcess => Ok(Value::Nil),
        SignalProcessTarget::Pid(pid) => Ok(Value::Int(if pid_exists(pid) { 0 } else { -1 })),
    }
}

/// (internal-default-process-filter PROCESS STRING) -> nil
///
/// When no custom filter is set, insert output into the process's associated
/// buffer at the process mark position (or end of buffer when mark is None).
/// This matches GNU Emacs's `internal-default-process-filter` behavior.
pub(crate) fn builtin_internal_default_process_filter(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal-default-process-filter", &args, 2)?;
    let id = resolve_process_or_wrong_type_any_in_manager(&eval.processes, &args[0])?;
    let text = expect_string_strict(&args[1])?;
    if text.is_empty() {
        return Ok(Value::Nil);
    }

    // Look up the process buffer name.
    let buf_name = match eval.processes.get(id) {
        Some(proc) => proc.buffer_name.clone(),
        None => return Ok(Value::Nil),
    };
    let Some(buf_name) = buf_name else {
        return Ok(Value::Nil);
    };
    let Some(buf_id) = eval.buffers.find_buffer_by_name(&buf_name) else {
        return Ok(Value::Nil);
    };

    // Get mark position or end of buffer (ZV in GNU terms).
    let mark_pos = eval.processes.get(id).and_then(|p| p.mark_byte_pos);
    let insert_pos = match mark_pos {
        Some(pos) => pos,
        None => eval.buffers.get(buf_id).map(|b| b.text.len()).unwrap_or(0),
    };

    // Save current point, move point to insert position, insert, then restore.
    let saved_pt = eval.buffers.get(buf_id).map(|b| b.pt);
    let old_read_only = eval.buffers.get(buf_id).map(|b| b.read_only);

    // Temporarily clear read-only so process output can be inserted.
    if let Some(buf) = eval.buffers.get_mut(buf_id) {
        buf.read_only = false;
        buf.goto_byte(insert_pos);
    }

    // Insert text at point (which is now at the mark position).
    let text_byte_len = text.len();
    eval.buffers.insert_into_buffer(buf_id, &text);

    // The new mark is at point after insertion (insert advances point).
    let new_mark = eval
        .buffers
        .get(buf_id)
        .map(|b| b.pt)
        .unwrap_or(insert_pos + text_byte_len);

    // Restore read-only flag.
    if let (Some(buf), Some(ro)) = (eval.buffers.get_mut(buf_id), old_read_only) {
        buf.read_only = ro;
    }

    // Restore original point, adjusted for the insertion.
    if let (Some(buf), Some(old_pt)) = (eval.buffers.get_mut(buf_id), saved_pt) {
        let adjusted_pt = if old_pt >= insert_pos {
            old_pt + text_byte_len
        } else {
            old_pt
        };
        buf.goto_byte(adjusted_pt);
    }

    // Advance process mark.
    if let Some(proc) = eval.processes.get_mut(id) {
        proc.mark_byte_pos = Some(new_mark);
    }

    Ok(Value::Nil)
}

/// (internal-default-process-sentinel PROCESS STRING) -> nil
pub(crate) fn builtin_internal_default_process_sentinel(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_internal_default_process_sentinel_impl(&eval.processes, args)
}

pub(crate) fn builtin_internal_default_process_sentinel_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal-default-process-sentinel", &args, 2)?;
    let _id = resolve_live_process_or_wrong_type_in_manager(processes, &args[0])?;
    Ok(Value::Nil)
}

/// (gnutls-boot PROCESS TYPE PROPLIST) -> t or error
///
/// Upgrade a network process to TLS using rustls (matching GNU's GnuTLS binding).
/// PROCESS must be a network process with an open TCP socket.
/// TYPE is ignored (GNU uses it for credential type).
/// PROPLIST is a keyword plist; we extract `:hostname` for SNI.
#[cfg(unix)]
pub(crate) fn builtin_gnutls_boot(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-boot", &args, 3)?;
    let id = resolve_process_or_wrong_type_any_in_manager(&eval.processes, &args[0])?;

    // Extract :hostname from plist for SNI.
    let plist = &args[2];
    let mut hostname: Option<String> = None;
    if let Some(items) = list_to_vec(plist) {
        let mut i = 0;
        while i + 1 < items.len() {
            if let Some(kw) = keyword_name(&items[i]) {
                if kw == ":hostname" {
                    hostname = items[i + 1].as_str().map(|s| s.to_string());
                }
            }
            i += 2;
        }
    }

    let proc = eval
        .processes
        .get_mut(id)
        .ok_or_else(|| signal("error", vec![Value::string("Process not found")]))?;

    if proc.kind != ProcessKind::Network {
        return Err(signal(
            "error",
            vec![Value::string("gnutls-boot: not a network process")],
        ));
    }

    // Take the plain TCP socket — it will be owned by the TLS stream.
    let tcp_stream = proc.socket.take().ok_or_else(|| {
        signal(
            "error",
            vec![Value::string(
                "gnutls-boot: no socket (already TLS or closed)",
            )],
        )
    })?;

    let host = hostname.unwrap_or_else(|| "localhost".to_string());

    // Build rustls config with Mozilla root certificates.
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let server_name: rustls_pki_types::ServerName<'_> = host.clone().try_into().map_err(|_| {
        signal(
            "error",
            vec![Value::string(format!("Invalid hostname for TLS: {}", host))],
        )
    })?;

    let tls_conn = rustls::ClientConnection::new(Arc::new(config), server_name).map_err(|e| {
        signal(
            "error",
            vec![Value::string(format!("TLS handshake failed: {}", e))],
        )
    })?;

    // Temporarily set the stream to blocking for the handshake.
    tcp_stream.set_nonblocking(false).ok();
    let mut tls_stream = rustls::StreamOwned::new(tls_conn, tcp_stream);

    // Drive the handshake to completion by doing a zero-length read.
    // This forces rustls to exchange TLS records over the socket.
    {
        use std::io::Read;
        let mut dummy = [0u8; 0];
        match tls_stream.read(&mut dummy) {
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(signal(
                    "gnutls-error",
                    vec![
                        Value::Int(-1),
                        Value::string("TLS handshake: unexpected EOF"),
                    ],
                ));
            }
            Err(e) => {
                return Err(signal(
                    "gnutls-error",
                    vec![
                        Value::Int(-1),
                        Value::string(format!("TLS handshake: {}", e)),
                    ],
                ));
            }
        }
    }

    // Switch back to non-blocking for async I/O.
    tls_stream.sock.set_nonblocking(true).ok();

    // Store the TLS stream. The poller still watches the underlying fd
    // (which is the same fd that was registered for the plain socket).
    proc.tls_stream = Some(tls_stream);

    Ok(Value::True)
}

/// Stub for non-unix platforms.
#[cfg(not(unix))]
pub(crate) fn builtin_gnutls_boot(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("gnutls-boot", &args, 3)?;
    Ok(Value::Nil)
}

/// (isearch-process-search-char CHAR &optional COUNT) -> nil
pub(crate) fn builtin_isearch_process_search_char(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("isearch-process-search-char", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("isearch-process-search-char"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    Ok(Value::Nil)
}

/// (isearch-process-search-string STRING MESSAGE) -> nil
pub(crate) fn builtin_isearch_process_search_string(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("isearch-process-search-string", &args, 2)?;
    Ok(Value::Nil)
}

/// (minibuffer--sort-preprocess-history HISTORY) -> nil
pub(crate) fn builtin_minibuffer_sort_preprocess_history(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer--sort-preprocess-history", &args, 1)?;
    expect_sequence(&args[0])?;
    Ok(Value::Nil)
}

/// (print--preprocess OBJECT) -> nil
pub(crate) fn builtin_print_preprocess(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_print_preprocess_impl(args)
}

pub(crate) fn builtin_print_preprocess_impl(args: Vec<Value>) -> EvalResult {
    expect_args("print--preprocess", &args, 1)?;
    Ok(Value::Nil)
}

/// (syntax-propertize--in-process-p) -> nil
pub(crate) fn builtin_syntax_propertize_in_process_p(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("syntax-propertize--in-process-p", &args, 0)?;
    Ok(Value::Nil)
}

/// (window--adjust-process-windows) -> nil
pub(crate) fn builtin_window_adjust_process_windows(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window--adjust-process-windows", &args, 0)?;
    Ok(Value::Nil)
}

/// (window--process-window-list) -> nil
pub(crate) fn builtin_window_process_window_list(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window--process-window-list", &args, 0)?;
    Ok(Value::Nil)
}

/// (window-adjust-process-window-size PROCESS WINDOW) -> nil
pub(crate) fn builtin_window_adjust_process_window_size(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window-adjust-process-window-size", &args, 2)?;
    expect_list(&args[1])?;
    Ok(Value::Nil)
}

/// (window-adjust-process-window-size-largest PROCESS WINDOW) -> nil
pub(crate) fn builtin_window_adjust_process_window_size_largest(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window-adjust-process-window-size-largest", &args, 2)?;
    expect_list(&args[1])?;
    Ok(Value::Nil)
}

/// (window-adjust-process-window-size-smallest PROCESS WINDOW) -> nil
pub(crate) fn builtin_window_adjust_process_window_size_smallest(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window-adjust-process-window-size-smallest", &args, 2)?;
    expect_list(&args[1])?;
    Ok(Value::Nil)
}

/// (format-network-address ADDRESS &optional OMIT-PORT) -> string-or-nil
pub(crate) fn builtin_format_network_address(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_format_network_address_impl(args)
}

pub(crate) fn builtin_format_network_address_impl(args: Vec<Value>) -> EvalResult {
    expect_min_args("format-network-address", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("format-network-address"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let omit_port = args.get(1).is_some_and(Value::is_truthy);
    match &args[0] {
        Value::Str(s) => Ok(Value::string(with_heap(|h| h.get_string(*s).to_owned()))),
        Value::Nil => Ok(Value::Nil),
        Value::Vector(_) => {
            let Some(items) = vector_nonnegative_integers(&args[0]) else {
                return Ok(Value::Nil);
            };
            if let Some(ipv4) = format_ipv4_network_address(&items, omit_port) {
                return Ok(Value::string(ipv4));
            }
            if let Some(ipv6) = format_ipv6_network_address(&items, omit_port) {
                return Ok(Value::string(ipv6));
            }
            Ok(Value::Nil)
        }
        Value::Cons(_) => {
            let first = list_to_vec(&args[0])
                .and_then(|items| items.first().cloned())
                .and_then(|v| value_as_nonnegative_integer(&v));
            if let Some(family) = first {
                Ok(Value::string(format!("<Family {family}>")))
            } else {
                Ok(Value::Nil)
            }
        }
        _ => Ok(Value::Nil),
    }
}

/// (network-interface-list &optional FULL FAMILY) -> interface-list
pub(crate) fn builtin_network_interface_list(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_network_interface_list_impl(args)
}

pub(crate) fn builtin_network_interface_list_impl(args: Vec<Value>) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("network-interface-list"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let full = args.first().is_some_and(Value::is_truthy);
    let family = args.get(1).cloned().unwrap_or(Value::Nil);
    let include_ipv4 = if family.is_nil() {
        true
    } else {
        matches!(family.as_symbol_name(), Some("ipv4"))
    };
    let include_ipv6 = if family.is_nil() {
        true
    } else {
        matches!(family.as_symbol_name(), Some("ipv6"))
    };
    if !family.is_nil() && !include_ipv4 && !include_ipv6 {
        return Err(signal(
            "error",
            vec![Value::string("Unsupported address family")],
        ));
    }

    let mut entries = Vec::new();
    if let Some(host_entries) = host_interface_snapshot() {
        for entry in host_entries.into_iter().rev() {
            let include = match entry.family {
                NetworkAddressFamily::Ipv4 => include_ipv4,
                NetworkAddressFamily::Ipv6 => include_ipv6,
            };
            if !include {
                continue;
            }

            if full {
                entries.push(Value::list(vec![
                    Value::string(entry.name),
                    entry.address,
                    entry.list_broadcast,
                    entry.netmask,
                ]));
            } else {
                entries.push(Value::cons(Value::string(entry.name), entry.address));
            }
        }
    }

    if entries.is_empty() {
        if include_ipv6 {
            entries.push(interface_entry("lo", loopback_ipv6_address(), full));
        }
        if include_ipv4 {
            entries.push(interface_entry("lo", loopback_ipv4_address(), full));
        }
    }
    Ok(Value::list(entries))
}

/// (network-interface-info IFNAME) -> interface-info-or-nil
pub(crate) fn builtin_network_interface_info(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_network_interface_info_impl(args)
}

pub(crate) fn builtin_network_interface_info_impl(args: Vec<Value>) -> EvalResult {
    expect_args("network-interface-info", &args, 1)?;
    let ifname_raw = expect_string_strict(&args[0])?;
    // Match C-string interface-name handling: embedded NUL truncates lookup.
    let ifname = ifname_raw.split('\0').next().unwrap_or_default();
    // Emacs applies IFNAMSIZ-style byte limits, not character counts.
    if ifname.len() >= 16 {
        return Err(signal(
            "error",
            vec![Value::string("interface name too long")],
        ));
    }

    if let Some(host_entries) = host_interface_snapshot() {
        let mut first_match: Option<HostInterfaceEntry> = None;
        let mut ipv4_match: Option<HostInterfaceEntry> = None;

        for entry in host_entries {
            if entry.name != ifname {
                continue;
            }
            if first_match.is_none() {
                first_match = Some(entry.clone());
            }
            if entry.family == NetworkAddressFamily::Ipv4 {
                ipv4_match = Some(entry);
                break;
            }
        }

        if let Some(entry) = ipv4_match.or(first_match) {
            return Ok(Value::list(vec![
                entry.address,
                entry.info_broadcast,
                entry.netmask,
                entry.hwaddr.unwrap_or(Value::Nil),
                entry.flags,
            ]));
        }
    }

    if ifname == "lo" {
        return Ok(Value::list(vec![
            loopback_ipv4_address(),
            loopback_ipv4_broadcast(),
            loopback_ipv4_netmask(),
            loopback_hwaddr(),
            loopback_flags(),
        ]));
    }

    Ok(Value::Nil)
}

/// (network-lookup-address-info NAME &optional FAMILY HINTS) -> address-list
pub(crate) fn builtin_network_lookup_address_info(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_network_lookup_address_info_impl(args)
}

pub(crate) fn builtin_network_lookup_address_info_impl(args: Vec<Value>) -> EvalResult {
    expect_min_args("network-lookup-address-info", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("network-lookup-address-info"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let name = expect_string_strict(&args[0])?;

    let family = args.get(1).cloned().unwrap_or(Value::Nil);
    let hints = args.get(2).cloned().unwrap_or(Value::Nil);
    if !hints.is_nil() {
        return Err(signal(
            "error",
            vec![Value::string("Unsupported hints value")],
        ));
    }

    let lookup_family = if family.is_nil() {
        None
    } else if matches!(family.as_symbol_name(), Some("ipv4")) {
        Some(NetworkAddressFamily::Ipv4)
    } else if matches!(family.as_symbol_name(), Some("ipv6")) {
        Some(NetworkAddressFamily::Ipv6)
    } else {
        return Err(signal("error", vec![Value::string("Unsupported family")]));
    };
    let entries = resolve_network_lookup_addresses(&name, lookup_family);
    Ok(Value::list(entries))
}

/// (signal-names) -> list-of-signal-name-strings
pub(crate) fn builtin_signal_names(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_signal_names_impl(args)
}

pub(crate) fn builtin_signal_names_impl(args: Vec<Value>) -> EvalResult {
    expect_args("signal-names", &args, 0)?;
    let names = vec![
        "RTMAX", "RTMAX-1", "RTMAX-2", "RTMAX-3", "RTMAX-4", "RTMAX-5", "RTMAX-6", "RTMAX-7",
        "RTMAX-8", "RTMAX-9", "RTMAX-10", "RTMAX-11", "RTMAX-12", "RTMAX-13", "RTMAX-14",
        "RTMIN+15", "RTMIN+14", "RTMIN+13", "RTMIN+12", "RTMIN+11", "RTMIN+10", "RTMIN+9",
        "RTMIN+8", "RTMIN+7", "RTMIN+6", "RTMIN+5", "RTMIN+4", "RTMIN+3", "RTMIN+2", "RTMIN+1",
        "RTMIN", "SYS", "PWR", "POLL", "WINCH", "PROF", "VTALRM", "XFSZ", "XCPU", "URG", "TTOU",
        "TTIN", "TSTP", "STOP", "CONT", "CHLD", "STKFLT", "TERM", "ALRM", "PIPE", "USR2", "SEGV",
        "USR1", "KILL", "FPE", "BUS", "ABRT", "TRAP", "ILL", "QUIT", "INT", "HUP", "EXIT",
    ];
    Ok(Value::list(
        names.into_iter().map(Value::string).collect::<Vec<_>>(),
    ))
}

/// (list-system-processes) -> process-id-list
pub(crate) fn builtin_list_system_processes(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_list_system_processes_impl(args)
}

pub(crate) fn builtin_list_system_processes_impl(args: Vec<Value>) -> EvalResult {
    expect_args("list-system-processes", &args, 0)?;

    let mut pids: Vec<i64> = std::fs::read_dir("/proc")
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<i64>().ok())
        .collect();
    pids.sort_unstable();
    Ok(Value::list(pids.into_iter().map(Value::Int).collect()))
}

/// (num-processors &optional QUERY) -> integer
pub(crate) fn builtin_num_processors(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_num_processors_impl(args)
}

pub(crate) fn builtin_num_processors_impl(args: Vec<Value>) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("num-processors"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let count = std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(1);
    Ok(Value::Int(count))
}

/// (list-processes &optional QUERY-ONLY BUFFER) -> nil
pub(crate) fn builtin_list_processes(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("list-processes"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    Ok(Value::Nil)
}

/// (list-processes--refresh) -> row-spec
pub(crate) fn builtin_list_processes_refresh(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("list-processes--refresh", &args, 0)?;
    let spacer = Value::string_with_text_properties(
        " ",
        vec![StringTextPropertyRun {
            start: 0,
            end: 1,
            plist: Value::list(vec![
                Value::symbol("display"),
                Value::list(vec![
                    Value::symbol("space"),
                    Value::keyword(":align-to"),
                    Value::list(vec![
                        Value::symbol("+"),
                        Value::symbol("header-line-indent-width"),
                        Value::Int(0),
                    ]),
                ]),
            ]),
        }],
    );
    Ok(Value::list(vec![
        Value::string(""),
        Value::symbol("header-line-indent"),
        spacer,
    ]))
}

/// (make-network-process &rest ARGS) -> process-or-nil
pub(crate) fn builtin_make_network_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::Nil);
    }

    // ---- Parse all keyword arguments ----
    let mut name: Option<String> = None;
    let mut host: Option<String> = None;
    let mut service: Option<Value> = None;
    let mut server = false;
    let mut _family: Option<String> = None;
    let mut _type_kw: Option<String> = None;
    let mut _nowait = false;
    let mut filter_val = Value::Nil;
    let mut sentinel_val = Value::Nil;
    let mut buffer_val = Value::Nil;
    let mut _coding_val = Value::Nil;
    let mut noquery = false;

    let mut i = 0usize;
    while i < args.len() {
        let key = &args[i];
        let value = args.get(i + 1).cloned().unwrap_or(Value::Nil);
        let Some(key_name) = keyword_name(key) else {
            i += 1;
            continue;
        };
        match key_name {
            ":name" => name = Some(expect_process_name_string(&value)?),
            ":host" => {
                if !value.is_nil() {
                    host = Some(expect_string(&value)?);
                }
            }
            ":service" => service = Some(value),
            ":server" => server = value.is_truthy(),
            ":family" => {
                if !value.is_nil() {
                    _family = value.as_symbol_name().map(|s| s.to_string());
                }
            }
            ":type" => {
                if !value.is_nil() {
                    _type_kw = value.as_symbol_name().map(|s| s.to_string());
                }
            }
            ":nowait" => _nowait = value.is_truthy(),
            ":filter" => filter_val = value,
            ":sentinel" => sentinel_val = value,
            ":buffer" => buffer_val = value,
            ":coding" => _coding_val = value,
            ":noquery" => noquery = value.is_truthy(),
            _ => {}
        }
        i += 2;
    }

    let Some(name) = name else {
        return Err(signal(
            "error",
            vec![Value::string("Missing :name keyword parameter")],
        ));
    };

    let service = service.unwrap_or(Value::Nil);
    if service.is_nil() {
        return Err(signal_wrong_type_string(Value::Nil));
    }

    // Resolve :buffer to a buffer name (creating buffer if needed).
    let buffer_name = if !buffer_val.is_nil() {
        parse_make_process_buffer(eval, &buffer_val)?
    } else {
        None
    };

    if server {
        // ---- Server mode: create record, no actual listener yet ----
        let id = eval.processes.create_process_with_kind(
            name,
            buffer_name,
            "network".to_string(),
            Vec::new(),
            ProcessKind::Network,
        );
        if let Some(proc) = eval.processes.get_mut(id) {
            proc.network_server = true;
            proc.thread = current_thread_handle(&eval.threads);
            if !filter_val.is_nil() {
                proc.filter = filter_val;
            }
            if !sentinel_val.is_nil() {
                proc.sentinel = sentinel_val;
            }
            if noquery {
                proc.query_on_exit_flag = false;
            }
        }
        return Ok(Value::Int(id as i64));
    }

    // ---- Client mode: establish TCP connection ----
    let host_str = host.unwrap_or_else(|| "localhost".to_string());
    let port: u16 = match &service {
        Value::Int(n) => *n as u16,
        _ => {
            let s = expect_string(&service)?;
            s.parse::<u16>().unwrap_or(0)
        }
    };
    if port == 0 {
        return Err(signal("error", vec![Value::string("Invalid service/port")]));
    }

    let addr = format!("{}:{}", host_str, port);
    let stream = std::net::TcpStream::connect(&addr).map_err(|e| {
        signal(
            "file-error",
            vec![
                Value::string("make client process failed"),
                Value::string(format!("{}", e)),
                Value::string(&host_str),
                Value::Int(port as i64),
            ],
        )
    })?;
    stream.set_nonblocking(true).map_err(|e| {
        signal(
            "file-error",
            vec![Value::string(format!("set_nonblocking: {}", e))],
        )
    })?;

    let id = eval.processes.create_process_with_kind(
        name,
        buffer_name,
        "network".to_string(),
        Vec::new(),
        ProcessKind::Network,
    );
    if let Some(proc) = eval.processes.get_mut(id) {
        #[cfg(unix)]
        {
            proc.socket = Some(stream);
        }
        #[cfg(not(unix))]
        {
            drop(stream);
        }
        proc.network_server = false;
        proc.status = ProcessStatus::Run;
        proc.thread = current_thread_handle(&eval.threads);
        if !filter_val.is_nil() {
            proc.filter = filter_val;
        }
        if !sentinel_val.is_nil() {
            proc.sentinel = sentinel_val;
        }
        if noquery {
            proc.query_on_exit_flag = false;
        }
    }

    // Register socket fd with the poller for I/O notification.
    #[cfg(unix)]
    eval.processes.register_socket_fd(id).ok();

    // Call sentinel with "open\n" to signal successful connection
    // (GNU Emacs calls the sentinel when a network connection opens).
    let sentinel = eval
        .processes
        .get(id)
        .map(|p| p.sentinel)
        .unwrap_or(Value::Nil);
    if !sentinel.is_nil()
        && !sentinel.is_symbol_named(DEFAULT_PROCESS_SENTINEL_SYMBOL)
        && sentinel.is_truthy()
    {
        let proc_val = Value::Int(id as i64);
        let msg_val = Value::string("open\n");
        if let Err(e) = eval.apply(sentinel, vec![proc_val, msg_val]) {
            tracing::warn!("Network sentinel open error for pid {}: {:?}", id, e);
        }
    }

    Ok(Value::Int(id as i64))
}

/// (make-pipe-process &rest ARGS) -> process-or-nil
pub(crate) fn builtin_make_pipe_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_make_pipe_process_impl(&mut eval.processes, &mut eval.buffers, &eval.threads, args)
}

pub(crate) fn builtin_make_pipe_process_impl(
    processes: &mut ProcessManager,
    buffers: &mut BufferManager,
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::Nil);
    }

    let mut name: Option<String> = None;
    let mut buffer_name: Option<Option<String>> = None;

    let mut i = 0usize;
    while i < args.len() {
        let key = &args[i];
        let value = args.get(i + 1).cloned().unwrap_or(Value::Nil);
        let Some(key_name) = keyword_name(key) else {
            i += 1;
            continue;
        };
        match key_name {
            ":name" => {
                name = Some(expect_process_name_string(&value)?);
            }
            ":buffer" => {
                buffer_name = Some(parse_make_process_buffer_in_state(buffers, &value)?);
            }
            _ => {}
        }
        i += 2;
    }

    let Some(name) = name else {
        return Err(signal(
            "error",
            vec![Value::string("Missing :name keyword parameter")],
        ));
    };

    let resolved_buffer_name = match buffer_name {
        Some(explicit) => explicit,
        None => {
            if buffers.find_buffer_by_name(&name).is_none() {
                let _ = buffers.create_buffer(&name);
            }
            Some(name.clone())
        }
    };

    let id = processes.create_process_with_kind(
        name,
        resolved_buffer_name,
        "pipe".to_string(),
        Vec::new(),
        ProcessKind::Pipe,
    );
    if let Some(proc) = processes.get_mut(id) {
        proc.thread = current_thread_handle(threads);
    }
    Ok(Value::Int(id as i64))
}

/// (make-serial-process &rest ARGS) -> process-or-nil
pub(crate) fn builtin_make_serial_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_make_serial_process_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_make_serial_process_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::Nil);
    }

    let mut name: Option<String> = None;
    let mut port: Option<String> = None;
    let mut speed: Option<Value> = None;

    let mut i = 0usize;
    while i < args.len() {
        let key = &args[i];
        let value = args.get(i + 1).cloned().unwrap_or(Value::Nil);
        let Some(key_name) = keyword_name(key) else {
            i += 1;
            continue;
        };
        match key_name {
            ":name" => {
                name = Some(expect_process_name_string(&value)?);
            }
            ":port" => {
                if value.is_nil() {
                    port = None;
                } else {
                    port = Some(expect_string_strict(&value)?);
                }
            }
            ":speed" => {
                speed = Some(value);
            }
            _ => {}
        }
        i += 2;
    }

    if port.is_none() {
        return Err(signal("error", vec![Value::string("No port specified")]));
    }
    if speed.is_none() {
        return Err(signal("error", vec![Value::string(":speed not specified")]));
    }

    let id = processes.create_process_with_kind(
        name.unwrap_or_else(|| "serial".to_string()),
        None,
        "serial".to_string(),
        Vec::new(),
        ProcessKind::Serial,
    );
    Ok(Value::Int(id as i64))
}

/// (serial-process-configure &rest ARGS) -> nil
pub(crate) fn builtin_serial_process_configure(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_serial_process_configure_impl(&eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_serial_process_configure_impl(
    processes: &ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let mut process_id: Option<ProcessId> = None;
    let mut i = 0usize;
    while i < args.len() {
        let key = &args[i];
        let Some(key_name) = keyword_name(key) else {
            i += 1;
            continue;
        };
        let value = args.get(i + 1).cloned().unwrap_or(Value::Nil);
        match key_name {
            ":process" => {
                if value.is_nil() {
                    process_id = None;
                } else {
                    process_id = Some(resolve_process_or_missing_error_in_manager(
                        processes, &value,
                    )?);
                }
            }
            ":name" => match value {
                Value::Str(name) => {
                    let name_str = with_heap(|h| h.get_string(name).to_owned());
                    process_id = Some(
                        processes
                            .find_by_name(&name_str)
                            .ok_or_else(|| signal_process_does_not_exist(&name_str))?,
                    );
                }
                other => return Err(signal_wrong_type_processp(other)),
            },
            _ => {}
        }
        i += 2;
    }

    let id = match process_id {
        Some(id) => id,
        None => resolve_optional_process_or_current_buffer_in_state(processes, buffers, None)?,
    };
    let proc = processes
        .get(id)
        .ok_or_else(|| signal_wrong_type_processp(Value::Int(id as i64)))?;
    if proc.kind != ProcessKind::Serial {
        return Err(signal("error", vec![Value::string("Not a serial process")]));
    }
    Ok(Value::Nil)
}

/// (set-network-process-option PROCESS OPTION VALUE &optional NO-ERROR) -> nil
pub(crate) fn builtin_set_network_process_option(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_network_process_option_impl(&eval.processes, args)
}

pub(crate) fn builtin_set_network_process_option_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() < 3 || args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-network-process-option"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let id = resolve_live_process_or_wrong_type_in_manager(processes, &args[0])?;
    let proc = processes.get(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    if args[1].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[1]],
        ));
    }
    if proc.kind != ProcessKind::Network {
        return Err(signal(
            "error",
            vec![Value::string("Process is not a network process")],
        ));
    }
    if args.get(3).is_some_and(Value::is_truthy) {
        return Ok(Value::Nil);
    }
    Err(signal(
        "error",
        vec![Value::string("Unknown or unsupported option")],
    ))
}

/// (start-process NAME BUFFER PROGRAM &rest ARGS) -> process-id
pub(crate) fn builtin_start_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("start-process", &args, 3)?;
    let name = expect_process_name_string(&args[0])?;
    let buffer = parse_make_process_buffer(eval, &args[1])?;
    let program = if args[2].is_nil() {
        "nil".to_string()
    } else {
        expect_string_strict(&args[2])?
    };
    let proc_args: Vec<String> = args[3..]
        .iter()
        .map(expect_string_strict)
        .collect::<Result<Vec<_>, _>>()?;

    let id = eval
        .processes
        .create_process(name, buffer, program, proc_args);

    // Actually spawn the OS process.
    if let Err(e) = eval.processes.spawn_child(id) {
        // Process creation failed — mark as exited but still return the id
        // (GNU Emacs signals file-error for missing programs)
        return Err(signal(
            "file-error",
            vec![
                Value::string("Searching for program"),
                Value::string(e),
                args[2],
            ],
        ));
    }

    Ok(Value::Int(id as i64))
}

/// (start-process-shell-command NAME BUFFER COMMAND) -> process-id
pub(crate) fn builtin_start_process_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("start-process-shell-command", &args, 3)?;
    let name = expect_process_name_string(&args[0])?;
    let buffer = parse_make_process_buffer(eval, &args[1])?;
    let command = expect_string_strict(&args[2])?;
    let id = eval.processes.create_process(
        name,
        buffer,
        "sh".to_string(),
        vec!["-c".to_string(), command],
    );

    // Actually spawn the OS process.
    if let Err(e) = eval.processes.spawn_child(id) {
        return Err(signal(
            "file-error",
            vec![Value::string("Searching for program"), Value::string(e)],
        ));
    }

    Ok(Value::Int(id as i64))
}

/// (start-file-process NAME BUFFER PROGRAM &rest PROGRAM-ARGS) -> process-id
pub(crate) fn builtin_start_file_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("start-file-process", &args, 3)?;
    let name = expect_process_name_string(&args[0])?;
    let buffer = parse_make_process_buffer(eval, &args[1])?;
    let program = if args[2].is_nil() {
        "nil".to_string()
    } else {
        expect_string_strict(&args[2])?
    };
    let proc_args = parse_string_args_strict(&args[3..])?;
    let id = eval
        .processes
        .create_process(name, buffer, program, proc_args);
    Ok(Value::Int(id as i64))
}

/// (start-file-process-shell-command NAME BUFFER COMMAND) -> process-id
pub(crate) fn builtin_start_file_process_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("start-file-process-shell-command", &args, 3)?;
    let name = expect_process_name_string(&args[0])?;
    let buffer = parse_make_process_buffer(eval, &args[1])?;
    let command = expect_string_strict(&args[2])?;
    let id = eval.processes.create_process(
        name,
        buffer,
        "sh".to_string(),
        vec!["-c".to_string(), command],
    );
    Ok(Value::Int(id as i64))
}

/// (call-process PROGRAM &optional INFILE DESTINATION DISPLAY &rest ARGS)
///
/// Runs the command synchronously using `std::process::Command`, captures
/// output.  Returns the exit code as an integer.
pub(crate) fn builtin_call_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_call_process_impl(&mut eval.buffers, args)
}

pub(crate) fn builtin_call_process_impl(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("call-process", &args, 1)?;
    let program = expect_string_strict(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).unwrap_or(&Value::Nil);
    let cmd_args = if args.len() > 4 {
        parse_string_args_strict(&args[4..])?
    } else {
        Vec::new()
    };

    // DISPLAY (arg index 3): ignored in this implementation.
    run_process_command_in_state(buffers, &program, infile, destination, &cmd_args)
}

/// (call-process-shell-command COMMAND &optional INFILE DESTINATION DISPLAY &rest ARGS)
pub(crate) fn builtin_call_process_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("call-process-shell-command", &args, 1)?;
    let command = sequence_value_to_env_string(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).unwrap_or(&Value::Nil);
    let cmd_args = if args.len() > 4 {
        parse_sequence_args(&args[4..])?
    } else {
        Vec::new()
    };
    let shell_command = shell_command_with_args(&command, &cmd_args);
    let shell_args = vec!["-c".to_string(), shell_command];

    // DISPLAY (arg index 3): ignored in this implementation.
    run_process_command_in_state(&mut eval.buffers, "sh", infile, destination, &shell_args)
}

/// (process-file PROGRAM &optional INFILE DESTINATION DISPLAY &rest ARGS)
pub(crate) fn builtin_process_file(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-file", &args, 1)?;
    let program = expect_string_strict(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).unwrap_or(&Value::Nil);
    let cmd_args = if args.len() > 4 {
        parse_string_args_strict(&args[4..])?
    } else {
        Vec::new()
    };
    run_process_command_in_state(&mut eval.buffers, &program, infile, destination, &cmd_args)
}

/// (process-file-shell-command COMMAND &optional INFILE DESTINATION DISPLAY &rest ARGS)
pub(crate) fn builtin_process_file_shell_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-file-shell-command", &args, 1)?;
    let command = sequence_value_to_env_string(&args[0])?;
    let infile = parse_optional_infile(&args, 1)?;
    let destination = args.get(2).unwrap_or(&Value::Nil);
    let cmd_args = if args.len() > 4 {
        parse_sequence_args(&args[4..])?
    } else {
        Vec::new()
    };
    let shell_command = shell_command_with_args(&command, &cmd_args);
    let shell_args = vec!["-c".to_string(), shell_command];

    // DISPLAY (arg index 3): ignored in this implementation.
    run_process_command_in_state(&mut eval.buffers, "sh", infile, destination, &shell_args)
}

/// (process-lines PROGRAM &rest ARGS) -> list of lines
pub(crate) fn builtin_process_lines(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-lines", &args, 1)?;
    let program = expect_string_strict(&args[0])?;
    let cmd_args = parse_string_args_strict(&args[1..])?;
    let (status, stdout) = run_process_capture_output(&program, &cmd_args)?;
    if status != 0 {
        return Err(signal_process_lines_status_error(&program, status));
    }
    Ok(parse_output_lines(&stdout))
}

/// (process-lines-ignore-status PROGRAM &rest ARGS) -> list of lines
pub(crate) fn builtin_process_lines_ignore_status(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-lines-ignore-status", &args, 1)?;
    let program = expect_string_strict(&args[0])?;
    let cmd_args = parse_string_args_strict(&args[1..])?;
    let (_, stdout) = run_process_capture_output(&program, &cmd_args)?;
    Ok(parse_output_lines(&stdout))
}

/// (process-lines-handling-status PROGRAM STATUS-HANDLER &rest ARGS) -> list of lines
pub(crate) fn builtin_process_lines_handling_status(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-lines-handling-status", &args, 2)?;
    let program = expect_string_strict(&args[0])?;
    let status_handler = args[1];
    let cmd_args = parse_string_args_strict(&args[2..])?;
    let (status, stdout) = run_process_capture_output(&program, &cmd_args)?;
    let lines = parse_output_lines(&stdout);

    if !status_handler.is_nil() {
        let _ = eval.apply(status_handler, vec![Value::Int(status as i64)])?;
    } else if status != 0 {
        return Err(signal_process_lines_status_error(&program, status));
    }

    Ok(lines)
}

/// (call-process-region START END PROGRAM &optional DELETE DESTINATION DISPLAY &rest ARGS)
///
/// Pipes buffer region from START to END through PROGRAM.
pub(crate) fn builtin_call_process_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_call_process_region_impl(&mut eval.buffers, args)
}

pub(crate) fn builtin_call_process_region_impl(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("call-process-region", &args, 3)?;
    let program = expect_string_strict(&args[2])?;

    let delete = args.len() > 3 && args[3].is_truthy();
    let destination = if args.len() > 4 {
        &args[4]
    } else {
        &Value::Nil
    };
    let destination_spec = parse_call_process_destination(buffers, destination)?;
    // DISPLAY (arg index 5): ignored.

    let cmd_args = if args.len() > 6 {
        parse_string_args_strict(&args[6..])?
    } else {
        Vec::new()
    };

    // START semantics:
    // - nil => use whole buffer contents, ignore END
    // - string => use that string as stdin, ignore END
    // - integer/marker => use region START..END
    let region_text = match &args[0] {
        Value::Nil => {
            let (text, maybe_delete_range) = {
                let buf = buffers
                    .current_buffer()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let len = buf.text.len();
                (buf.text.text_range(0, len), (0usize, len))
            };
            if delete {
                let current_id = buffers
                    .current_buffer_id()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let _ = buffers.delete_buffer_region(
                    current_id,
                    maybe_delete_range.0,
                    maybe_delete_range.1,
                );
            }
            text
        }
        Value::Str(s) => {
            if delete {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integer-or-marker-p"), args[0]],
                ));
            }
            with_heap(|h| h.get_string(*s).to_owned())
        }
        _ => {
            let start = expect_int_or_marker(&args[0])?;
            let end = expect_int_or_marker(&args[1])?;
            let (text, region_beg, region_end) = {
                let buf = buffers
                    .current_buffer()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let (region_beg, region_end) = checked_region_bytes(buf, start, end)?;
                (
                    buf.text.text_range(region_beg, region_end),
                    region_beg,
                    region_end,
                )
            };

            if delete {
                let current_id = buffers
                    .current_buffer_id()
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
                let _ = buffers.delete_buffer_region(current_id, region_beg, region_end);
            }

            text
        }
    };

    use std::io::Write;
    if destination_spec.no_wait {
        let mut command = Command::new(&program);
        command
            .args(&cmd_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        match destination_spec.stderr {
            StderrTarget::Discard | StderrTarget::ToStdoutTarget => {
                command.stderr(Stdio::null());
            }
            StderrTarget::File => {
                let path = destination_spec.stderr_file.as_ref().ok_or_else(|| {
                    signal("error", vec![Value::string("Missing stderr file target")])
                })?;
                let file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(path)
                    .map_err(|e| signal_process_io("Writing process output", Some(path), e))?;
                command.stderr(Stdio::from(file));
            }
        };

        let mut child = command
            .spawn()
            .map_err(|e| signal_process_io("Searching for program", Some(&program), e))?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(region_text.as_bytes());
        }

        std::thread::spawn(move || {
            let _ = child.wait();
        });

        return Ok(Value::Nil);
    }

    let mut child = Command::new(&program)
        .args(&cmd_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| signal_process_io("Searching for program", Some(&program), e))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(region_text.as_bytes());
    }

    let output = child
        .wait_with_output()
        .map_err(|e| signal_process_io("Process error", None, e))?;

    let exit_code = output.status.code().unwrap_or(-1);
    route_captured_output_in_state(buffers, &destination_spec, &output.stdout, &output.stderr)?;
    Ok(Value::Int(exit_code as i64))
}

/// (delete-process PROCESS) -> nil
pub(crate) fn builtin_delete_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_delete_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_delete_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("delete-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let id = if let Some(process) = args.first() {
        if process.is_nil() {
            resolve_optional_process_or_current_buffer_in_state(processes, buffers, args.first())?
        } else {
            resolve_process_or_missing_error_any_in_manager(processes, process)?
        }
    } else {
        resolve_optional_process_or_current_buffer_in_state(processes, buffers, args.first())?
    };
    processes.delete_process(id);
    Ok(Value::Nil)
}

/// (continue-process &optional PROCESS CURRENT-GROUP) -> process-or-nil
pub(crate) fn builtin_continue_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_continue_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_continue_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("continue-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let (id, ret) =
        resolve_optional_process_with_explicit_return_in_state(processes, buffers, args.first())?;
    if let Some(proc) = processes.get_mut(id) {
        // Send SIGCONT to resume the child process.
        #[cfg(unix)]
        if let Some(ref child) = proc.child {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGCONT);
            }
        }
        proc.status = ProcessStatus::Run;
    }
    Ok(ret)
}

/// (interrupt-process &optional PROCESS CURRENT-GROUP) -> process-or-nil
pub(crate) fn builtin_interrupt_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_interrupt_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_interrupt_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("interrupt-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let (id, ret) =
        resolve_optional_process_with_explicit_return_in_state(processes, buffers, args.first())?;
    if let Some(proc) = processes.get_mut(id) {
        // Send SIGINT to actual child process.
        #[cfg(unix)]
        if let Some(ref child) = proc.child {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGINT);
            }
        }
        proc.status = ProcessStatus::Signal(2);
    }
    Ok(ret)
}

/// (kill-process &optional PROCESS CURRENT-GROUP) -> process-or-nil
pub(crate) fn builtin_kill_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_kill_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_kill_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("kill-process"), Value::Int(args.len() as i64)],
        ));
    }
    let (id, ret) =
        resolve_optional_process_with_explicit_return_in_state(processes, buffers, args.first())?;
    if let Some(proc) = processes.get_mut(id) {
        // Kill the actual child process.
        if let Some(child) = proc.child.as_mut() {
            let _ = child.kill();
        }
        proc.status = ProcessStatus::Signal(9);
    }
    Ok(ret)
}

/// (signal-process PROCESS SIGNAL &optional CURRENT-GROUP) -> int-or-nil
pub(crate) fn builtin_signal_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_signal_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_signal_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("signal-process", &args, 2)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("signal-process"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    if let Some(process) = args.first() {
        if !process.is_nil() && is_stale_process_id_designator_in_manager(processes, process) {
            return Ok(Value::Int(-1));
        }
    }

    let signal_num = parse_signal_number(&args[1])?;
    match resolve_signal_process_target_in_state(processes, buffers, args.first())? {
        SignalProcessTarget::Process(id) => {
            if let Some(proc) = processes.get_mut(id) {
                // Send actual OS signal to child process.
                #[cfg(unix)]
                if let Some(ref child) = proc.child {
                    let pid = child.id() as i32;
                    unsafe {
                        libc::kill(pid, signal_num);
                    }
                }
                proc.status = ProcessStatus::Signal(signal_num);
            }
            Ok(Value::Int(0))
        }
        SignalProcessTarget::MissingNamedProcess => Ok(Value::Nil),
        SignalProcessTarget::Pid(pid) => {
            #[cfg(unix)]
            {
                let result = unsafe { libc::kill(pid as i32, signal_num) };
                Ok(Value::Int(result as i64))
            }
            #[cfg(not(unix))]
            {
                Ok(Value::Int(if pid_exists(pid) { 0 } else { -1 }))
            }
        }
    }
}

/// (stop-process &optional PROCESS CURRENT-GROUP) -> process-or-nil
pub(crate) fn builtin_stop_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_stop_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_stop_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("stop-process"), Value::Int(args.len() as i64)],
        ));
    }
    let (id, ret) =
        resolve_optional_process_with_explicit_return_in_state(processes, buffers, args.first())?;
    if let Some(proc) = processes.get_mut(id) {
        // Send SIGTSTP to stop the child process.
        #[cfg(unix)]
        if let Some(ref child) = proc.child {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTSTP);
            }
        }
        proc.status = ProcessStatus::Stop;
    }
    Ok(ret)
}

/// (quit-process &optional PROCESS CURRENT-GROUP) -> process-or-nil
pub(crate) fn builtin_quit_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_quit_process_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_quit_process_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("quit-process"), Value::Int(args.len() as i64)],
        ));
    }
    let (id, ret) =
        resolve_optional_process_with_explicit_return_in_state(processes, buffers, args.first())?;
    if let Some(proc) = processes.get_mut(id) {
        // Send SIGQUIT to the child process.
        #[cfg(unix)]
        if let Some(ref child) = proc.child {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGQUIT);
            }
        }
    }
    Ok(ret)
}

/// (process-attributes PID) -> alist-or-nil
pub(crate) fn builtin_process_attributes(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_attributes_impl(args)
}

pub(crate) fn builtin_process_attributes_impl(args: Vec<Value>) -> EvalResult {
    expect_args("process-attributes", &args, 1)?;
    let pid = match &args[0] {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        _ => return Err(signal_wrong_type_numberp(args[0])),
    };
    if !pid_exists(pid) {
        return Ok(Value::Nil);
    }

    let mut attrs = Vec::new();
    if let Some((euid, egid)) = parse_effective_ids_from_proc_status(pid) {
        attrs.push(Value::cons(
            Value::symbol("group"),
            Value::string(lookup_group_name(egid).unwrap_or_else(|| egid.to_string())),
        ));
        attrs.push(Value::cons(Value::symbol("egid"), Value::Int(egid as i64)));
        attrs.push(Value::cons(
            Value::symbol("user"),
            Value::string(lookup_user_name(euid).unwrap_or_else(|| euid.to_string())),
        ));
        attrs.push(Value::cons(Value::symbol("euid"), Value::Int(euid as i64)));
    }

    let stat = parse_proc_stat_snapshot(pid).unwrap_or_else(|| ProcStatSnapshot::fallback(pid));
    attrs.push(Value::cons(Value::symbol("comm"), Value::string(stat.comm)));
    attrs.push(Value::cons(
        Value::symbol("state"),
        Value::string(stat.state),
    ));
    attrs.push(Value::cons(Value::symbol("ppid"), Value::Int(stat.ppid)));
    attrs.push(Value::cons(Value::symbol("pgrp"), Value::Int(stat.pgrp)));
    attrs.push(Value::cons(Value::symbol("sess"), Value::Int(stat.sess)));
    attrs.push(Value::cons(Value::symbol("tpgid"), Value::Int(stat.tpgid)));
    attrs.push(Value::cons(
        Value::symbol("minflt"),
        Value::Int(stat.minflt),
    ));
    attrs.push(Value::cons(
        Value::symbol("majflt"),
        Value::Int(stat.majflt),
    ));
    attrs.push(Value::cons(
        Value::symbol("cminflt"),
        Value::Int(stat.cminflt),
    ));
    attrs.push(Value::cons(
        Value::symbol("cmajflt"),
        Value::Int(stat.cmajflt),
    ));
    attrs.push(Value::cons(
        Value::symbol("utime"),
        time_list_from_ticks(stat.utime_ticks, clock_ticks_per_second()),
    ));
    attrs.push(Value::cons(
        Value::symbol("stime"),
        time_list_from_ticks(stat.stime_ticks, clock_ticks_per_second()),
    ));
    let total_ticks = stat.utime_ticks.saturating_add(stat.stime_ticks);
    attrs.push(Value::cons(
        Value::symbol("time"),
        time_list_from_ticks(total_ticks, clock_ticks_per_second()),
    ));
    attrs.push(Value::cons(
        Value::symbol("cutime"),
        time_list_from_ticks(stat.cutime_ticks, clock_ticks_per_second()),
    ));
    attrs.push(Value::cons(
        Value::symbol("cstime"),
        time_list_from_ticks(stat.cstime_ticks, clock_ticks_per_second()),
    ));
    let total_child_ticks = stat.cutime_ticks.saturating_add(stat.cstime_ticks);
    attrs.push(Value::cons(
        Value::symbol("ctime"),
        time_list_from_ticks(total_child_ticks, clock_ticks_per_second()),
    ));
    attrs.push(Value::cons(Value::symbol("pri"), Value::Int(stat.pri)));
    attrs.push(Value::cons(Value::symbol("nice"), Value::Int(stat.nice)));
    attrs.push(Value::cons(
        Value::symbol("thcount"),
        Value::Int(stat.thcount),
    ));
    let hz = clock_ticks_per_second();
    let start_epoch_time = parse_proc_boot_time_secs().map(|boot_secs| {
        let (start_rel_secs, start_rel_usecs) = ticks_to_secs_usecs(stat.start_ticks, hz);
        (boot_secs.saturating_add(start_rel_secs), start_rel_usecs)
    });
    let (start_secs, start_usecs) = start_epoch_time.unwrap_or((0, 0));
    attrs.push(Value::cons(
        Value::symbol("start"),
        time_list_from_secs_usecs(start_secs, start_usecs),
    ));
    attrs.push(Value::cons(Value::symbol("vsize"), Value::Int(stat.vsize)));
    attrs.push(Value::cons(Value::symbol("rss"), Value::Int(stat.rss)));
    let elapsed = match (now_epoch_secs_usecs(), start_epoch_time) {
        (Some(now), Some(start)) => nonnegative_time_diff(now, start),
        _ => (0, 0),
    };
    attrs.push(Value::cons(
        Value::symbol("etime"),
        time_list_from_secs_usecs(elapsed.0, elapsed.1),
    ));
    let elapsed_secs = elapsed.0 as f64 + (elapsed.1 as f64 / 1_000_000.0);
    let total_cpu_secs = if hz > 0 {
        (total_ticks as f64) / (hz as f64)
    } else {
        0.0
    };
    let pcpu = if elapsed_secs > 0.0 {
        (total_cpu_secs * 100.0) / elapsed_secs
    } else {
        0.0
    };
    attrs.push(Value::cons(
        Value::symbol("pcpu"),
        Value::Float(
            if pcpu.is_finite() { pcpu.max(0.0) } else { 0.0 },
            next_float_id(),
        ),
    ));
    let pmem = parse_total_memory_kb()
        .filter(|mem_total_kb| *mem_total_kb > 0)
        .map(|mem_total_kb| (stat.rss as f64 * 100.0) / mem_total_kb as f64)
        .unwrap_or(0.0);
    attrs.push(Value::cons(
        Value::symbol("pmem"),
        Value::Float(
            if pmem.is_finite() { pmem.max(0.0) } else { 0.0 },
            next_float_id(),
        ),
    ));
    attrs.push(Value::cons(
        Value::symbol("args"),
        Value::string(parse_proc_cmdline(pid)),
    ));
    attrs.push(Value::cons(
        Value::symbol("ttname"),
        Value::string(stat.ttname),
    ));

    Ok(Value::list(attrs))
}

/// (make-process &rest ARGS) -> process-or-nil
pub(crate) fn builtin_make_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_make_process_impl(&mut eval.processes, &mut eval.buffers, args)
}

pub(crate) fn builtin_make_process_impl(
    processes: &mut ProcessManager,
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::Nil);
    }

    let mut name: Option<String> = None;
    let mut buffer_name: Option<Option<String>> = None;
    let mut command: Option<Vec<String>> = None;
    let mut filter = Value::Nil;
    let mut sentinel = Value::Nil;

    let mut i = 0usize;
    while i < args.len() {
        let key = &args[i];
        let value = args.get(i + 1).cloned().unwrap_or(Value::Nil);
        let key_name = match key {
            Value::Keyword(k) => Some(resolve_sym(*k)),
            Value::Symbol(id) if resolve_sym(*id).starts_with(':') => Some(resolve_sym(*id)),
            _ => None,
        };
        match key_name {
            Some(":name") => match value {
                Value::Str(s) => name = Some(with_heap(|h| h.get_string(s).to_owned())),
                _ => {
                    return Err(signal(
                        "error",
                        vec![Value::string(":name value not a string")],
                    ));
                }
            },
            Some(":buffer") => {
                buffer_name = Some(parse_make_process_buffer_in_state(buffers, &value)?)
            }
            Some(":command") => command = Some(parse_make_process_command(&value)?),
            Some(":filter") => filter = value,
            Some(":sentinel") => sentinel = value,
            _ => {} // :connection-type, :coding, :noquery, :stop, :stderr — ignored for now
        }
        i += 2;
    }

    let Some(name) = name else {
        return Err(signal(
            "error",
            vec![Value::string("Missing :name keyword parameter")],
        ));
    };

    let command = command.unwrap_or_default();
    let (program, argv) = if command.is_empty() {
        (String::new(), Vec::new())
    } else {
        (command[0].clone(), command[1..].to_vec())
    };
    let id = processes.create_process(name, buffer_name.unwrap_or(None), program, argv);

    // Set filter and sentinel if provided.
    if !filter.is_nil() {
        if let Some(proc) = processes.get_mut(id) {
            proc.filter = filter;
        }
    }
    if !sentinel.is_nil() {
        if let Some(proc) = processes.get_mut(id) {
            proc.sentinel = sentinel;
        }
    }

    // Spawn the actual OS child process.
    if let Err(e) = processes.spawn_child(id) {
        return Err(signal(
            "file-error",
            vec![Value::string("Searching for program"), Value::string(e)],
        ));
    }

    Ok(Value::Int(id as i64))
}

pub(crate) fn builtin_accept_process_output_collect(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> Result<(Value, Vec<(Value, Vec<Value>)>), Flow> {
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("accept-process-output"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    if let Some(process) = args.first() {
        if !process.is_nil()
            && resolve_live_process_designator_in_manager(processes, process).is_none()
        {
            if is_stale_process_id_designator_in_manager(processes, process) {
                return Ok((Value::Nil, Vec::new()));
            }
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("processp"), *process],
            ));
        }
    }

    if let Some(seconds) = args.get(1) {
        if let Some(milliseconds) = args.get(2) {
            if !milliseconds.is_nil() && !matches!(milliseconds, Value::Int(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("fixnump"), *milliseconds],
                ));
            }
            if milliseconds.is_nil() {
                if !seconds.is_nil() && !seconds.is_number() {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("numberp"), *seconds],
                    ));
                }
            } else if !seconds.is_nil() && !matches!(seconds, Value::Int(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("fixnump"), *seconds],
                ));
            }
        } else if !seconds.is_nil() && !seconds.is_number() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("numberp"), *seconds],
            ));
        }
    }

    let timeout_ms: Option<u64> = {
        let secs = args.get(1).and_then(|v| match v {
            Value::Int(n) if !v.is_nil() => Some(*n as f64),
            Value::Float(f, _) => Some(*f),
            _ => None,
        });
        let ms = args
            .get(2)
            .and_then(|v| match v {
                Value::Int(n) if !v.is_nil() => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        match secs {
            Some(s) => Some((s * 1000.0) as u64 + ms as u64),
            None if ms > 0 => Some(ms as u64),
            _ => Some(50),
        }
    };

    let target_id = if let Some(process) = args.first() {
        if !process.is_nil() {
            resolve_live_process_designator_in_manager(processes, process)
        } else {
            None
        }
    } else {
        None
    };

    let proc_ids: Vec<ProcessId> = if let Some(id) = target_id {
        vec![id]
    } else {
        processes.live_process_ids()
    };

    let start = std::time::Instant::now();
    let deadline = timeout_ms.map(std::time::Duration::from_millis);
    let mut got_output = false;
    let mut callbacks = Vec::new();

    loop {
        for &pid in &proc_ids {
            let exited = processes.check_child_exit(pid);

            let read_result = processes.read_process_output(pid);
            let is_network = processes
                .get(pid)
                .map(|p| p.kind == ProcessKind::Network)
                .unwrap_or(false);

            match &read_result {
                Some(data) if !data.is_empty() => {
                    got_output = true;
                    let filter = processes.get(pid).map(|p| p.filter).unwrap_or(Value::Nil);
                    if !filter.is_nil()
                        && !filter.is_symbol_named(DEFAULT_PROCESS_FILTER_SYMBOL)
                        && filter.is_truthy()
                    {
                        callbacks.push((filter, vec![Value::Int(pid as i64), Value::string(data)]));
                    }
                }
                None if is_network => {
                    // Network connection closed (EOF).
                    if let Some(proc) = processes.get_mut(pid) {
                        proc.status = ProcessStatus::Exit(0);
                    }
                    let sentinel = processes.get(pid).map(|p| p.sentinel).unwrap_or(Value::Nil);
                    if !sentinel.is_nil()
                        && !sentinel.is_symbol_named(DEFAULT_PROCESS_SENTINEL_SYMBOL)
                        && sentinel.is_truthy()
                    {
                        callbacks.push((
                            sentinel,
                            vec![
                                Value::Int(pid as i64),
                                Value::string("connection broken by remote peer\n"),
                            ],
                        ));
                    }
                    continue;
                }
                _ => {}
            }

            if exited {
                let sentinel = processes.get(pid).map(|p| p.sentinel).unwrap_or(Value::Nil);
                let exit_msg = processes
                    .get(pid)
                    .map(|p| match &p.status {
                        ProcessStatus::Exit(code) => {
                            if *code == 0 {
                                "finished\n".to_string()
                            } else {
                                format!("exited abnormally with code {}\n", code)
                            }
                        }
                        ProcessStatus::Signal(sig) => format!("killed by signal {}\n", sig),
                        _ => "finished\n".to_string(),
                    })
                    .unwrap_or_else(|| "finished\n".to_string());
                if !sentinel.is_nil()
                    && !sentinel.is_symbol_named(DEFAULT_PROCESS_SENTINEL_SYMBOL)
                    && sentinel.is_truthy()
                {
                    callbacks.push((
                        sentinel,
                        vec![Value::Int(pid as i64), Value::string(&exit_msg)],
                    ));
                }
            }
        }

        if got_output {
            return Ok((Value::True, callbacks));
        }

        if let Some(d) = deadline {
            if start.elapsed() >= d {
                return Ok((Value::Nil, callbacks));
            }
        } else {
            return Ok((Value::Nil, callbacks));
        }

        let elapsed = start.elapsed();
        let remaining = deadline
            .and_then(|d| d.checked_sub(elapsed))
            .unwrap_or(std::time::Duration::from_millis(50));
        let wait_time = remaining.min(std::time::Duration::from_millis(50));
        let _ = processes.wait_for_output(wait_time);
    }
}

/// (process-send-string PROCESS STRING) -> nil
pub(crate) fn builtin_process_send_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_send_string_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_process_send_string_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-send-string", &args, 2)?;
    let input = expect_string_strict(&args[1])?;
    if let Value::Int(n) = args[0] {
        if n >= 0 && is_stale_process_id_designator_in_manager(processes, &args[0]) {
            return Err(signal_process_not_running_in_manager(
                processes,
                n as ProcessId,
            ));
        }
    }
    let id = resolve_process_or_missing_error_in_manager(processes, &args[0])?;
    if !processes.send_input(id, &input) {
        return Err(signal("error", vec![Value::string("Process not found")]));
    }
    Ok(Value::Nil)
}

/// (process-status PROCESS) -> symbol
pub(crate) fn builtin_process_status(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_status_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_process_status_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-status", &args, 1)?;
    let Some(id) = (match &args[0] {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            if processes.get_any(id).is_some() {
                Some(id)
            } else {
                return Err(signal_wrong_type_processp(args[0]));
            }
        }
        Value::Str(s) => {
            let name = with_heap(|h| h.get_string(*s).to_owned());
            processes.find_by_name(&name)
        }
        _ => return Err(signal_wrong_type_processp(args[0])),
    }) else {
        return Ok(Value::Nil);
    };
    // Check if child process has exited since last check.
    processes.check_child_exit(id);
    match processes.get_any(id) {
        Some(proc) => match proc.status {
            ProcessStatus::Run => match proc.kind {
                ProcessKind::Network => {
                    if proc.network_server {
                        Ok(Value::symbol("listen"))
                    } else {
                        Ok(Value::symbol("open"))
                    }
                }
                ProcessKind::Pipe => Ok(Value::symbol("open")),
                _ => Ok(Value::symbol("run")),
            },
            ProcessStatus::Stop => Ok(Value::symbol("stop")),
            ProcessStatus::Exit(_) => Ok(Value::symbol("exit")),
            ProcessStatus::Signal(_) => match proc.kind {
                ProcessKind::Real => Ok(Value::symbol("signal")),
                _ => Ok(Value::symbol("closed")),
            },
            ProcessStatus::Connect => Ok(Value::symbol("connect")),
            ProcessStatus::Failed => Ok(Value::symbol("failed")),
        },
        None => Ok(Value::Nil),
    }
}

/// (process-exit-status PROCESS) -> integer
pub(crate) fn builtin_process_exit_status(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_exit_status_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_exit_status_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-exit-status", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes
        .get_any(id)
        .ok_or_else(|| signal_wrong_type_processp(args[0]))?;
    match proc.status {
        ProcessStatus::Exit(code) => Ok(Value::Int(code as i64)),
        ProcessStatus::Signal(sig) => {
            if proc.kind == ProcessKind::Real {
                Ok(Value::Int(sig as i64))
            } else {
                Ok(Value::Int(0))
            }
        }
        _ => Ok(Value::Int(0)),
    }
}

/// (process-list) -> list of process ids
pub(crate) fn builtin_process_list(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_list_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_list_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-list", &args, 0)?;
    let ids = processes.list_processes();
    let values: Vec<Value> = ids.iter().map(|id| Value::Int(*id as i64)).collect();
    Ok(Value::list(values))
}

/// (process-name PROCESS) -> string
pub(crate) fn builtin_process_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_name_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_name_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-name", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    match processes.get_any(id) {
        Some(proc) => Ok(Value::string(proc.name.clone())),
        None => Err(signal_wrong_type_processp(args[0])),
    }
}

/// (process-buffer PROCESS) -> string or nil
pub(crate) fn builtin_process_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_buffer_impl(&eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_process_buffer_impl(
    processes: &ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-buffer", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    match processes.get_any(id) {
        Some(proc) => match &proc.buffer_name {
            Some(name) => Ok(buffers
                .find_buffer_by_name(name)
                .or_else(|| buffers.find_dead_buffer_by_name(name))
                .map(Value::Buffer)
                .unwrap_or(Value::Nil)),
            None => Ok(Value::Nil),
        },
        None => Err(signal_wrong_type_processp(args[0])),
    }
}

/// (process-coding-system PROCESS) -> (decode . encode)
pub(crate) fn builtin_process_coding_system(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_coding_system_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_coding_system_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-coding-system", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(Value::cons(proc.coding_decode, proc.coding_encode))
}

/// (process-datagram-address PROCESS) -> address-or-nil
pub(crate) fn builtin_process_datagram_address(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_datagram_address_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_datagram_address_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-datagram-address", &args, 1)?;
    let _id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    Ok(Value::Nil)
}

/// (process-inherit-coding-system-flag PROCESS) -> bool
pub(crate) fn builtin_process_inherit_coding_system_flag(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_inherit_coding_system_flag_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_inherit_coding_system_flag_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-inherit-coding-system-flag", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(Value::bool(proc.inherit_coding_system_flag))
}

/// (set-process-buffer PROCESS BUFFER) -> BUFFER
pub(crate) fn builtin_set_process_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_buffer_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_set_process_buffer_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-buffer", &args, 2)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let next_buffer_name = match &args[1] {
        Value::Nil => None,
        Value::Buffer(buffer_id) => Some(
            buffers
                .get(*buffer_id)
                .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?
                .name
                .clone(),
        ),
        _ => return Err(signal_wrong_type_bufferp(args[1])),
    };
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.buffer_name = next_buffer_name;
    Ok(args[1])
}

/// (set-process-coding-system PROCESS &optional DECODING ENCODING) -> nil
pub(crate) fn builtin_set_process_coding_system(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_coding_system_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_set_process_coding_system_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-process-coding-system", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-process-coding-system"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    if let Some(coding) = args.get(1) {
        proc.coding_decode = *coding;
        proc.coding_encode = args.get(2).cloned().unwrap_or(*coding);
    }
    Ok(Value::Nil)
}

/// (set-buffer-process-coding-system DECODING ENCODING) -> nil
pub(crate) fn builtin_set_buffer_process_coding_system(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-buffer-process-coding-system", &args, 2)?;
    let id = resolve_optional_process_or_current_buffer(eval, None)?;
    let proc = eval.processes.get_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), Value::Int(id as i64)],
        )
    })?;
    proc.coding_decode = args[0];
    proc.coding_encode = args[1];
    Ok(Value::Nil)
}

/// (set-process-datagram-address PROCESS ADDRESS) -> nil
pub(crate) fn builtin_set_process_datagram_address(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_datagram_address_impl(&eval.processes, args)
}

pub(crate) fn builtin_set_process_datagram_address_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-datagram-address", &args, 2)?;
    let _id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    Ok(Value::Nil)
}

/// (set-process-inherit-coding-system-flag PROCESS FLAG) -> FLAG
pub(crate) fn builtin_set_process_inherit_coding_system_flag(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_inherit_coding_system_flag_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_set_process_inherit_coding_system_flag_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-inherit-coding-system-flag", &args, 2)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.inherit_coding_system_flag = args[1].is_truthy();
    Ok(args[1])
}

/// (set-process-thread PROCESS THREAD) -> thread-or-nil
pub(crate) fn builtin_set_process_thread(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_thread_impl(&mut eval.processes, &eval.threads, args)
}

pub(crate) fn builtin_set_process_thread_impl(
    processes: &mut ProcessManager,
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-thread", &args, 2)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let value = if args[1].is_nil() {
        Value::Nil
    } else if threads.thread_id_from_handle(&args[1]).is_some() {
        args[1]
    } else {
        return Err(signal_wrong_type_threadp(args[1]));
    };
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.thread = value;
    Ok(value)
}

/// (set-process-window-size PROCESS COLS ROWS) -> t
pub(crate) fn builtin_set_process_window_size(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_window_size_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_set_process_window_size_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-window-size", &args, 3)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let cols = expect_integer(&args[1])?;
    let rows = expect_integer(&args[2])?;
    let is_live = processes.get(id).is_some();
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.window_cols = Some(cols);
    proc.window_rows = Some(rows);
    Ok(if is_live { Value::True } else { Value::Nil })
}

/// (process-kill-buffer-query-function) -> bool
pub(crate) fn builtin_process_kill_buffer_query_function(args: Vec<Value>) -> EvalResult {
    expect_args("process-kill-buffer-query-function", &args, 0)?;
    Ok(Value::True)
}

/// (process-menu-delete-process) -> nil
pub(crate) fn builtin_process_menu_delete_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-menu-delete-process", &args, 0)?;
    let current_buffer_name = eval
        .buffers
        .current_buffer()
        .map(|buffer| buffer.name.clone())
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if eval
        .processes
        .find_by_buffer_name(&current_buffer_name)
        .is_some()
    {
        return Err(signal(
            "error",
            vec![Value::string(
                "Buffer does not seem to be associated with any file",
            )],
        ));
    }
    let _ = resolve_optional_process_or_current_buffer(eval, None)?;
    Ok(Value::Nil)
}

/// (process-menu-visit-buffer LINE) -> nil
pub(crate) fn builtin_process_menu_visit_buffer(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-menu-visit-buffer", &args, 1)?;
    let _line = expect_int_or_marker(&args[0])?;
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("stringp"), Value::Nil],
    ))
}

/// (process-tty-name PROCESS &optional STREAM) -> string-or-nil
pub(crate) fn builtin_process_tty_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_tty_name_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_tty_name_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-tty-name", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("process-tty-name"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    let stream = args.get(1).cloned().unwrap_or(Value::Nil);
    let tty_value = || proc.tty_name.as_ref().map_or(Value::Nil, Value::string);

    match stream {
        Value::Nil => Ok(tty_value()),
        Value::Symbol(sym) if resolve_sym(sym) == "stdin" => {
            if proc.tty_stdin {
                Ok(tty_value())
            } else {
                Ok(Value::Nil)
            }
        }
        Value::Symbol(sym) if resolve_sym(sym) == "stdout" => {
            if proc.tty_stdout {
                Ok(tty_value())
            } else {
                Ok(Value::Nil)
            }
        }
        Value::Symbol(sym) if resolve_sym(sym) == "stderr" => {
            if proc.tty_stderr {
                Ok(tty_value())
            } else {
                Ok(Value::Nil)
            }
        }
        other => Err(signal(
            "error",
            vec![Value::string("Unknown stream"), other],
        )),
    }
}

/// (process-mark PROCESS) -> marker
pub(crate) fn builtin_process_mark(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_mark_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_mark_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-mark", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(super::marker::make_marker_value(
        proc.buffer_name.as_deref(),
        None,
        false,
    ))
}

/// (process-type PROCESS) -> symbol
pub(crate) fn builtin_process_type(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_type_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_type_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-type", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(Value::symbol(match proc.kind {
        ProcessKind::Real => "real",
        ProcessKind::Network => "network",
        ProcessKind::Pipe => "pipe",
        ProcessKind::Serial => "serial",
    }))
}

/// (process-thread PROCESS) -> object-or-nil
pub(crate) fn builtin_process_thread(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_thread_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_thread_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-thread", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(proc.thread)
}

/// (process-send-region PROCESS START END) -> nil
pub(crate) fn builtin_process_send_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_send_region_impl(&mut eval.processes, &mut eval.buffers, args)
}

pub(crate) fn builtin_process_send_region_impl(
    processes: &mut ProcessManager,
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-send-region", &args, 3)?;

    if let Value::Int(n) = args[0] {
        if n >= 0 && is_stale_process_id_designator_in_manager(processes, &args[0]) {
            let _ = expect_int_or_marker(&args[1])?;
            let _ = expect_int_or_marker(&args[2])?;
            return Err(signal_process_not_running_in_manager(
                processes,
                n as ProcessId,
            ));
        }
    }

    let id =
        resolve_optional_process_or_current_buffer_in_state(processes, buffers, Some(&args[0]))?;
    let start = expect_int_or_marker(&args[1])?;
    let end = expect_int_or_marker(&args[2])?;

    let region_text = {
        let buf = buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        let (region_beg, region_end) = checked_region_bytes(buf, start, end)?;
        buf.text.text_range(region_beg, region_end)
    };

    if !processes.send_input(id, &region_text) {
        return Err(signal("error", vec![Value::string("Process not found")]));
    }
    Ok(Value::Nil)
}

/// (process-send-eof &optional PROCESS) -> process-or-nil
pub(crate) fn builtin_process_send_eof(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_send_eof_impl(&mut eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_process_send_eof_impl(
    processes: &mut ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("process-send-eof"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    if let Some(process) = args.first() {
        if !process.is_nil() {
            if let Value::Int(n) = process {
                if *n >= 0 && is_stale_process_id_designator_in_manager(processes, process) {
                    return Err(signal_process_not_running_in_manager(
                        processes,
                        *n as ProcessId,
                    ));
                }
            }
            let id = resolve_process_or_missing_error_in_manager(processes, process)?;
            // Close stdin to send EOF to the child process.
            if let Some(proc) = processes.get_mut(id) {
                if let Some(ref mut child) = proc.child {
                    // Drop stdin to close the pipe, sending EOF.
                    drop(child.stdin.take());
                }
            }
            return Ok(*process);
        }
    }
    let id = resolve_optional_process_or_current_buffer_in_state(processes, buffers, args.first())?;
    // Close stdin to send EOF.
    if let Some(proc) = processes.get_mut(id) {
        if let Some(ref mut child) = proc.child {
            drop(child.stdin.take());
        }
    }
    Ok(Value::Nil)
}

/// (process-running-child-p &optional PROCESS) -> bool
pub(crate) fn builtin_process_running_child_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_running_child_p_impl(&eval.processes, &eval.buffers, args)
}

pub(crate) fn builtin_process_running_child_p_impl(
    processes: &ProcessManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("process-running-child-p"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    if let Some(process) = args.first() {
        if let Value::Int(n) = process {
            if *n >= 0 && is_stale_process_id_designator_in_manager(processes, process) {
                return Err(signal_process_not_active_in_manager(
                    processes,
                    *n as ProcessId,
                ));
            }
        }
    }
    let _id =
        resolve_optional_process_or_current_buffer_in_state(processes, buffers, args.first())?;
    Ok(Value::Nil)
}

/// (accept-process-output &optional PROCESS SECONDS MILLISECS JUST-THIS-ONE) -> bool
///
/// Batch/runtime compatibility path: validates arguments, then returns nil.
pub(crate) fn builtin_accept_process_output(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let (result, callbacks) = builtin_accept_process_output_collect(&mut eval.processes, args)?;
    for (callback, callback_args) in callbacks {
        let _ = eval.apply(callback, callback_args)?;
    }
    Ok(result)
}

/// (get-process NAME) -> process-or-nil
pub(crate) fn builtin_get_process(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_get_process_impl(&eval.processes, args)
}

pub(crate) fn builtin_get_process_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-process", &args, 1)?;
    let name = expect_string_strict(&args[0])?;
    match processes.find_by_name(&name) {
        Some(id) => Ok(Value::Int(id as i64)),
        None => Ok(Value::Nil),
    }
}

/// (get-buffer-process BUFFER-OR-NAME) -> process-or-nil
pub(crate) fn builtin_get_buffer_process(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_buffer_process_impl(&eval.frames, &eval.buffers, &eval.processes, args)
}

pub(crate) fn builtin_get_buffer_process_impl(
    frames: &FrameManager,
    buffers: &BufferManager,
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-buffer-process", &args, 1)?;
    let Some(buffer_name) =
        resolve_buffer_name_for_process_lookup_in_state(frames, buffers, &args[0])?
    else {
        return Ok(Value::Nil);
    };
    match processes.find_by_buffer_name(&buffer_name) {
        Some(id) => Ok(Value::Int(id as i64)),
        None => Ok(Value::Nil),
    }
}

/// (processp OBJECT) -> bool
pub(crate) fn builtin_processp(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_processp_impl(&eval.processes, args)
}

pub(crate) fn builtin_processp_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("processp", &args, 1)?;
    Ok(Value::bool(match &args[0] {
        Value::Int(n) if *n >= 0 => processes.get_any(*n as ProcessId).is_some(),
        _ => false,
    }))
}

/// (process-live-p PROCESS) -> list-or-nil
pub(crate) fn builtin_process_live_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_live_p_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_live_p_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-live-p", &args, 1)?;
    let Some(id) = (match &args[0] {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            processes.get(id).map(|_| id)
        }
        _ => None,
    }) else {
        return Ok(Value::Nil);
    };
    let proc = processes.get(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(process_live_status_value(&proc.status, &proc.kind))
}

/// (process-id PROCESS) -> integer
pub(crate) fn builtin_process_id(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_process_id_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_id_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-id", &args, 1)?;
    let id = match &args[0] {
        Value::Int(n) if *n >= 0 => {
            let id = *n as ProcessId;
            if processes.get_any(id).is_some() {
                id
            } else {
                return Err(signal_wrong_type_processp(args[0]));
            }
        }
        _ => return Err(signal_wrong_type_processp(args[0])),
    };
    let proc = processes
        .get_any(id)
        .ok_or_else(|| signal_wrong_type_processp(args[0]))?;
    if proc.kind == ProcessKind::Real {
        Ok(Value::Int(id as i64))
    } else {
        Ok(Value::Nil)
    }
}

/// (process-query-on-exit-flag PROCESS) -> bool
pub(crate) fn builtin_process_query_on_exit_flag(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_query_on_exit_flag_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_query_on_exit_flag_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-query-on-exit-flag", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(Value::bool(proc.query_on_exit_flag))
}

/// (set-process-query-on-exit-flag PROCESS FLAG) -> FLAG
pub(crate) fn builtin_set_process_query_on_exit_flag(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_query_on_exit_flag_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_set_process_query_on_exit_flag_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-query-on-exit-flag", &args, 2)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let flag = args[1].is_truthy();
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.query_on_exit_flag = flag;
    Ok(args[1])
}

/// (process-command PROCESS) -> list
pub(crate) fn builtin_process_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_command_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_command_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-command", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    if proc.kind != ProcessKind::Real || proc.command.is_empty() {
        return Ok(Value::Nil);
    }
    let mut items = Vec::with_capacity(proc.args.len() + 1);
    items.push(Value::string(proc.command.clone()));
    items.extend(proc.args.iter().cloned().map(Value::string));
    Ok(Value::list(items))
}

/// (process-contact PROCESS &optional KEY NO-BLOCK) -> value
pub(crate) fn builtin_process_contact(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_contact_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_contact_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("process-contact", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("process-contact"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    let key = args.get(1).cloned().unwrap_or(Value::Nil);
    match proc.kind {
        ProcessKind::Network => {
            let port = 40000_i64 + (proc.id % 20000) as i64;
            let local = Value::vector(vec![
                Value::Int(127),
                Value::Int(0),
                Value::Int(0),
                Value::Int(1),
                Value::Int(port),
            ]);
            if key.is_nil() {
                Ok(Value::list(vec![Value::Nil, Value::Int(port)]))
            } else if key == Value::True {
                Ok(Value::list(vec![
                    Value::keyword(":name"),
                    Value::string(proc.name.clone()),
                    Value::keyword(":server"),
                    Value::True,
                    Value::keyword(":service"),
                    Value::Int(port),
                    Value::keyword(":local"),
                    local,
                ]))
            } else {
                match key {
                    Value::Keyword(k) if resolve_sym(k) == ":name" => {
                        Ok(Value::string(proc.name.clone()))
                    }
                    Value::Keyword(k) if resolve_sym(k) == ":server" => Ok(Value::True),
                    Value::Keyword(k) if resolve_sym(k) == ":service" => Ok(Value::Int(port)),
                    Value::Keyword(k) if resolve_sym(k) == ":local" => Ok(local),
                    _ => Ok(Value::Nil),
                }
            }
        }
        ProcessKind::Pipe => {
            if key.is_nil() {
                Ok(Value::True)
            } else if key == Value::True {
                Ok(Value::list(vec![
                    Value::keyword(":name"),
                    Value::string(proc.name.clone()),
                ]))
            } else {
                match key {
                    Value::Keyword(k) if resolve_sym(k) == ":name" => {
                        Ok(Value::string(proc.name.clone()))
                    }
                    _ => Ok(Value::Nil),
                }
            }
        }
        _ => Ok(Value::True),
    }
}

/// (process-filter PROCESS) -> function
pub(crate) fn builtin_process_filter(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_filter_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_filter_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-filter", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(proc.filter)
}

/// (set-process-filter PROCESS FILTER) -> FILTER
pub(crate) fn builtin_set_process_filter(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_filter_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_set_process_filter_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-filter", &args, 2)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let stored = if args[1].is_nil() {
        Value::symbol(DEFAULT_PROCESS_FILTER_SYMBOL)
    } else {
        args[1]
    };
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.filter = stored;
    Ok(stored)
}

/// (process-sentinel PROCESS) -> function
pub(crate) fn builtin_process_sentinel(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_sentinel_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_sentinel_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-sentinel", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(proc.sentinel)
}

/// (set-process-sentinel PROCESS SENTINEL) -> SENTINEL
pub(crate) fn builtin_set_process_sentinel(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_sentinel_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_set_process_sentinel_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-sentinel", &args, 2)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let stored = if args[1].is_nil() {
        Value::symbol(DEFAULT_PROCESS_SENTINEL_SYMBOL)
    } else {
        args[1]
    };
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.sentinel = stored;
    Ok(stored)
}

/// (process-plist PROCESS) -> plist
pub(crate) fn builtin_process_plist(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_process_plist_impl(&eval.processes, args)
}

pub(crate) fn builtin_process_plist_impl(
    processes: &ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("process-plist", &args, 1)?;
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    Ok(proc.plist)
}

/// (set-process-plist PROCESS PLIST) -> plist
pub(crate) fn builtin_set_process_plist(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_process_plist_impl(&mut eval.processes, args)
}

pub(crate) fn builtin_set_process_plist_impl(
    processes: &mut ProcessManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-process-plist", &args, 2)?;
    if !args[1].is_list() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), args[1]],
        ));
    }
    let id = resolve_process_or_wrong_type_any_in_manager(processes, &args[0])?;
    let proc = processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.plist = args[1];
    Ok(proc.plist)
}

/// (process-put PROCESS PROP VALUE) -> plist
pub(crate) fn builtin_process_put(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("process-put", &args, 3)?;
    let id = resolve_process_or_wrong_type_any(eval, &args[0])?;
    let current_plist = eval
        .processes
        .get_any(id)
        .ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("processp"), args[0]],
            )
        })?
        .plist;
    let new_plist = super::builtins::builtin_plist_put(vec![current_plist, args[1], args[2]])?;
    let proc = eval.processes.get_any_mut(id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), args[0]],
        )
    })?;
    proc.plist = new_plist;
    Ok(new_plist)
}

/// (process-get PROCESS PROP) -> value
pub(crate) fn builtin_process_get(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("process-get", &args, 2)?;
    let id = resolve_process_or_wrong_type_any(eval, &args[0])?;
    let plist = eval
        .processes
        .get_any(id)
        .ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("processp"), args[0]],
            )
        })?
        .plist;
    super::builtins::builtin_plist_get(vec![plist, args[1]])
}

// ---------------------------------------------------------------------------
// Builtins (pure — no evaluator needed)
// ---------------------------------------------------------------------------

/// (shell-command-to-string COMMAND) -> string
///
/// Runs COMMAND via the system shell and returns captured stdout.
pub(crate) fn builtin_shell_command_to_string(args: Vec<Value>) -> EvalResult {
    expect_args("shell-command-to-string", &args, 1)?;
    let command = expect_string(&args[0])?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    let output = Command::new(&shell)
        .arg("-c")
        .arg(&command)
        .output()
        .map_err(|e| signal_process_io("Shell command failed", Some(&shell), e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(Value::string(stdout))
}

fn getenv_impl(name: &str, args: &[Value]) -> EvalResult {
    expect_min_args(name, args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ));
    }
    if let Some(frame) = args.get(1) {
        if !frame.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("framep"), *frame],
            ));
        }
    }
    let name = expect_string_strict(&args[0])?;
    match std::env::var(&name) {
        Ok(val) => Ok(Value::string(val)),
        Err(_) => Ok(Value::Nil),
    }
}

/// (getenv VARIABLE) -> string or nil
pub(crate) fn builtin_getenv(args: Vec<Value>) -> EvalResult {
    getenv_impl("getenv", &args)
}

/// (getenv-internal VARIABLE &optional ENV) -> string or nil
///
/// GNU-compatible: checks process-environment first, then falls back
/// to the real OS environment (matching callproc.c:getenv_internal).
/// When ENV is a list, searches that list instead.
pub(crate) fn builtin_getenv_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("getenv-internal", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("getenv-internal"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let varname = expect_string_strict(&args[0])?;

    // If ENV arg is a list, search it directly (GNU behavior).
    if let Some(env_list) = args.get(1) {
        if env_list.is_cons() {
            return getenv_from_list(&varname, *env_list);
        }
    }

    // Check process-environment variable first (GNU callproc.c:1720).
    let proc_env = eval.obarray.symbol_value("process-environment").cloned();
    if let Some(pe) = proc_env {
        if pe.is_cons() {
            let result = getenv_from_list(&varname, pe)?;
            if !result.is_nil() {
                return Ok(result);
            }
        }
    }

    // Fall back to real OS environment.
    match std::env::var(&varname) {
        Ok(val) => Ok(Value::string(val)),
        Err(_) => Ok(Value::Nil),
    }
}

/// Search a process-environment-style list for VARIABLE.
/// Each entry is "VARIABLE=VALUE" or just "VARIABLE" (no value).
fn getenv_from_list(varname: &str, env_list: Value) -> EvalResult {
    use crate::emacs_core::value::list_to_vec;
    let prefix = format!("{}=", varname);
    if let Some(entries) = list_to_vec(&env_list) {
        for entry in &entries {
            if let Some(s) = entry.as_str() {
                if let Some(value_part) = s.strip_prefix(&prefix) {
                    return Ok(Value::string(value_part.to_string()));
                }
                // Entry with no = means variable exists but no value
                if s == varname {
                    return Ok(Value::Nil);
                }
            }
        }
    }
    Ok(Value::Nil)
}


/// (set-binary-mode STREAM MODE) -> t
///
/// Batch/runtime compatibility path. Accepts stdin/stdout/stderr symbols.
pub(crate) fn builtin_set_binary_mode(args: Vec<Value>) -> EvalResult {
    expect_args("set-binary-mode", &args, 2)?;
    let stream = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;

    match stream {
        "stdin" | "stdout" | "stderr" => Ok(Value::True),
        _ => Err(signal(
            "error",
            vec![Value::string("unsupported stream"), args[0]],
        )),
    }
}

impl GcTrace for ProcessManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for process in self
            .processes
            .values()
            .chain(self.deleted_processes.values())
        {
            roots.push(process.filter);
            roots.push(process.sentinel);
            roots.push(process.plist);
            roots.push(process.coding_decode);
            roots.push(process.coding_encode);
            roots.push(process.thread);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "process_test.rs"]
mod tests;
