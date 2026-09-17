//! Named pipe endpoint for the shared local daemon on Windows.
//!
//! The daemon creates the pipe with the first-instance flag, which is the
//! election: a second daemon is refused with access denied. The pipe carries a
//! security descriptor that grants access to the current user alone, and it
//! refuses remote clients. The client opens the pipe with identification
//! quality of service, so the daemon cannot impersonate it.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
    ERROR_PIPE_BUSY, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::core::PWSTR;

use super::{BlockingStream, Election, Endpoint, Listener};

/// How many pipe instances listen at the same time.
const POOL_INSTANCES: usize = 4;

/// First wait between connection attempts.
const FIRST_BACKOFF: Duration = Duration::from_millis(10);

/// Longest wait between connection attempts.
const LAST_BACKOFF: Duration = Duration::from_millis(200);

/// The named pipe of one endpoint identity.
pub(super) fn pipe_name(id: &str) -> String {
    format!(r"\\.\pipe\semctl-{id}")
}

/// The daemon log file of one endpoint identity.
pub(super) fn log_path(id: &str) -> Result<PathBuf> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| anyhow!("no local application data directory for the daemon log"))?;
    Ok(base.join("semctl").join(format!("daemon-{id}.log")))
}

/// Encode a string as a null-terminated UTF-16 sequence.
fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// An open access token for this process.
struct ProcessToken(HANDLE);

impl ProcessToken {
    /// Open this process's token for reading.
    fn open() -> Result<Self> {
        let mut handle: HANDLE = ptr::null_mut();
        // SAFETY: `GetCurrentProcess` returns a pseudo handle that needs no
        // close, and `handle` is a writable slot for the new token handle.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut handle) };
        if opened == 0 {
            return Err(io::Error::last_os_error()).context("open the process access token");
        }
        Ok(Self(handle))
    }
}

impl Drop for ProcessToken {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcessToken` and is closed once,
        // because this value owns it.
        unsafe { CloseHandle(self.0) };
    }
}

/// A UTF-16 string the system allocated with `LocalAlloc`.
struct LocalString(PWSTR);

impl LocalString {
    /// Copy the string into owned storage.
    fn to_os_string(&self) -> OsString {
        let mut length = 0;
        // SAFETY: the pointer is a null-terminated UTF-16 string, so every
        // read up to and including the terminator is inside the allocation.
        while unsafe { *self.0.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: the first `length` code units are inside the allocation and
        // are not written while this borrow lives.
        let units = unsafe { std::slice::from_raw_parts(self.0, length) };
        OsString::from_wide(units)
    }
}

impl Drop for LocalString {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a call that allocates with
        // `LocalAlloc`, and this value frees it once.
        unsafe { LocalFree(self.0.cast()) };
    }
}

/// The security identifier string of the user this process runs as.
pub(super) fn current_user_sid() -> Result<String> {
    let token = ProcessToken::open()?;
    let mut needed: u32 = 0;
    // SAFETY: a null buffer of length zero asks only for the required size.
    // The call is expected to fail with `ERROR_INSUFFICIENT_BUFFER`.
    let sized =
        unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &raw mut needed) };
    if sized == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER.cast_signed()) {
            return Err(error).context("size the token user information");
        }
    }
    // `TOKEN_USER` holds a pointer, so the buffer must be pointer aligned.
    // Storage of `u64` satisfies that on every Windows target.
    let words = (needed as usize).div_ceil(size_of::<u64>()).max(1);
    let mut buffer = vec![0u64; words];
    let capacity = u32::try_from(words * size_of::<u64>()).unwrap_or(u32::MAX);
    // SAFETY: the buffer holds `capacity` writable bytes and stays alive for
    // the whole call.
    let read = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            capacity,
            &raw mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error()).context("read the token user information");
    }
    // SAFETY: the call filled the buffer with one `TOKEN_USER` whose `Sid`
    // points inside that same buffer, which is alive until this function ends.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_to_string(sid)
}

/// Format a security identifier in its string form.
fn sid_to_string(sid: PSID) -> Result<String> {
    let mut text: PWSTR = ptr::null_mut();
    // SAFETY: `sid` points at a valid security identifier inside a live
    // buffer, and `text` is a writable slot for the allocated string.
    let converted = unsafe { ConvertSidToStringSidW(sid, &raw mut text) };
    if converted == 0 {
        return Err(io::Error::last_os_error()).context("format the user security identifier");
    }
    LocalString(text)
        .to_os_string()
        .into_string()
        .map_err(|_| anyhow!("the user security identifier is not valid text"))
}

/// A security descriptor the system allocated with `LocalAlloc`.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the value owns the only pointer to its descriptor. The descriptor is
// never written after construction, and its only reader is the named pipe
// creation call, which takes it through a `SECURITY_ATTRIBUTES` structure.
unsafe impl Send for SecurityDescriptor {}
// SAFETY: see the `Send` implementation. Shared access is read only.
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    /// Grant full access to one security identifier and to nobody else.
    ///
    /// `D:P` marks the access control list protected, so no inherited entry
    /// can widen it. `(A;;GA;;;<sid>)` allows generic access to that user.
    fn for_user(sid: &str) -> Result<Self> {
        let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is a null-terminated UTF-16 string that outlives the
        // call, and `descriptor` is a writable slot for the result.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error())
                .context("build the endpoint security descriptor");
        }
        Ok(Self(descriptor))
    }

    /// The attributes structure that passes this descriptor to a new pipe.
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a call that allocates with
        // `LocalAlloc`, and this value frees it once.
        unsafe { LocalFree(self.0.cast()) };
    }
}

/// Create one listening pipe instance.
fn create_instance(
    name: &str,
    descriptor: &SecurityDescriptor,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut attributes = descriptor.attributes();
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte);
    // SAFETY: `attributes` is a valid `SECURITY_ATTRIBUTES` value that points
    // at a live security descriptor, and it outlives this call.
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
}

/// A pool of listening named pipe instances for one endpoint.
pub(crate) struct PipeListener {
    name: String,
    descriptor: SecurityDescriptor,
    ready: Mutex<VecDeque<NamedPipeServer>>,
}

impl PipeListener {
    /// The pool of listening instances.
    ///
    /// A poisoned lock is recovered: the pool is a plain queue of handles and
    /// a panic cannot leave it in an inconsistent state.
    fn pool(&self) -> MutexGuard<'_, VecDeque<NamedPipeServer>> {
        self.ready.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Create instances until the pool holds its share of the pool size.
    ///
    /// One further instance is always out with a caller that awaits a
    /// connection, so the pool itself holds one less than [`POOL_INSTANCES`].
    fn refill(&self, pool: &mut VecDeque<NamedPipeServer>) {
        while pool.len() < POOL_INSTANCES - 1 {
            match create_instance(&self.name, &self.descriptor, false) {
                Ok(instance) => pool.push_back(instance),
                Err(error) => {
                    // Best effort. The next accept creates an instance on
                    // demand, so a refill failure loses no connection.
                    tracing::debug!(%error, "could not pre-create a named pipe instance");
                    break;
                }
            }
        }
    }

    /// Take one listening instance and pre-create its replacement.
    fn take_ready(&self) -> Result<NamedPipeServer> {
        let mut pool = self.pool();
        let instance = match pool.pop_front() {
            Some(instance) => instance,
            None => create_instance(&self.name, &self.descriptor, false)
                .with_context(|| format!("create named pipe {}", self.name))?,
        };
        // The replacement exists before the caller awaits a connection, so a
        // client that arrives during that await finds a free instance.
        self.refill(&mut pool);
        Ok(instance)
    }

    /// Accept the next connection.
    ///
    /// The security descriptor limits the pipe to the endpoint owner, so an
    /// accepted peer already runs as that user.
    pub(super) async fn accept(&self) -> Result<NamedPipeServer> {
        let instance = self.take_ready()?;
        instance
            .connect()
            .await
            .with_context(|| format!("accept on {}", self.name))?;
        Ok(instance)
    }
}

/// Elect this process and create the endpoint pipe.
///
/// Call this inside a Tokio runtime: each instance registers with the reactor.
pub(super) fn bind(endpoint: &Endpoint) -> Result<Election> {
    let descriptor = SecurityDescriptor::for_user(endpoint.user_sid())?;
    let name = endpoint.pipe_name().to_string();
    let first = match create_instance(&name, &descriptor, true) {
        Ok(instance) => instance,
        // The first-instance flag is the election. Access denied means another
        // daemon already created this name.
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED.cast_signed()) => {
            return Ok(Election::Lost);
        }
        Err(error) => return Err(error).with_context(|| format!("create named pipe {name}")),
    };
    let mut ready = VecDeque::with_capacity(POOL_INSTANCES);
    ready.push_back(first);
    let listener = PipeListener {
        name,
        descriptor,
        ready: Mutex::new(ready),
    };
    {
        let mut pool = listener.pool();
        listener.refill(&mut pool);
    }
    Ok(Election::Won(Listener::Pipe(listener)))
}

/// Connect to the endpoint with blocking input and output.
///
/// The call retries a busy or missing pipe until `deadline`, because a daemon
/// this client just spawned needs a moment to create its instances.
pub(super) fn connect(endpoint: &Endpoint, deadline: Instant) -> Result<BlockingStream> {
    let name = endpoint.pipe_name();
    let mut backoff = FIRST_BACKOFF;
    loop {
        // Identification quality of service lets the daemon check the client's
        // identity but not act as the client.
        match OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
        {
            Ok(file) => return Ok(BlockingStream::Pipe(file)),
            Err(error) if is_absent(&error) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error)
                        .with_context(|| format!("connect {name} before the deadline"));
                }
                std::thread::sleep(backoff.min(remaining));
                backoff = (backoff * 2).min(LAST_BACKOFF);
            }
            Err(error) => return Err(error).with_context(|| format!("connect {name}")),
        }
    }
}

/// Whether the failure means no free daemon instance exists yet.
fn is_absent(error: &io::Error) -> bool {
    match error.raw_os_error() {
        Some(code) => {
            code == ERROR_PIPE_BUSY.cast_signed() || code == ERROR_FILE_NOT_FOUND.cast_signed()
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{SecurityDescriptor, current_user_sid, pipe_name, wide};

    #[test]
    fn the_pipe_name_is_local_and_carries_the_identity() {
        assert_eq!(
            pipe_name("0123456789abcdef"),
            r"\\.\pipe\semctl-0123456789abcdef"
        );
    }

    #[test]
    fn wide_strings_end_with_a_null_unit() {
        let encoded = wide("ab");
        assert_eq!(encoded, vec![u16::from(b'a'), u16::from(b'b'), 0]);
    }

    #[test]
    fn the_current_user_has_a_security_identifier() {
        let sid = current_user_sid().expect("read the user security identifier");
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[test]
    fn a_security_descriptor_grants_the_current_user_alone() {
        let sid = current_user_sid().expect("read the user security identifier");
        let descriptor = SecurityDescriptor::for_user(&sid).expect("build the descriptor");
        assert!(!descriptor.0.is_null());
    }

    #[test]
    fn a_malformed_security_identifier_is_refused() {
        let error = SecurityDescriptor::for_user("not-a-sid")
            .err()
            .expect("a malformed identifier is refused");
        assert!(error.to_string().contains("security descriptor"), "{error}");
    }
}
