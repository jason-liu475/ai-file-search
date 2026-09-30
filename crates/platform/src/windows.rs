use std::ffi::c_void;
use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::os::windows::io::AsRawHandle;
use std::ptr::{self, NonNull};

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, IsValidAcl, IsValidSecurityDescriptor, IsValidSid,
    OWNER_SECURITY_INFORMATION, SE_DACL_PRESENT, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const PIPE_PREFIX: &str = r"\\.\pipe\";
const _: () = assert!(align_of::<usize>() >= align_of::<TOKEN_USER>());

#[cfg(test)]
mod tests;

/// Creates a duplex local pipe with a protected DACL allowing only the process user's SID.
///
/// `name` must be a complete local name (`\\.\pipe\name`). Use `first_instance = true`
/// at the initial bind to reject a pre-existing pipe; use `false` only while an owned
/// instance remains alive. Every call supplies the descriptor to `CreateNamedPipeW`,
/// including calls creating subsequent instances. No post-creation ACL change occurs.
/// Remote clients are rejected and the server handle is not inheritable.
///
/// This is a user-account boundary, not isolation from other processes/sessions of
/// that user or privileged Windows security overrides. It uses the process token,
/// not a potentially impersonated thread token.
///
/// # Errors
///
/// Returns an error on an invalid local name, token/SID/descriptor failure, pipe
/// creation failure (including first-instance collision), or Tokio I/O registration
/// failure. It never falls back to the token's default DACL. Call within a Tokio
/// runtime with I/O enabled.
pub fn create_private_pipe(name: &str, first_instance: bool) -> io::Result<NamedPipeServer> {
    validate_name(name)?;
    let sid = current_user_sid()?;
    // GR/GW also grant FILE_CREATE_PIPE_INSTANCE, needed for our subsequent servers.
    // Explicit owner avoids an elevated token's default group owner gaining WRITE_DAC.
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GRGW;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: sddl is NUL-terminated and live for the call; descriptor is writable.
    // On success Windows returns a valid self-relative LocalAlloc descriptor.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let descriptor = LocalAllocation::from_success(descriptor)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| invalid_native_data("security attributes size overflow"))?,
        lpSecurityDescriptor: descriptor.0.as_ptr(),
        bInheritHandle: 0,
    };
    // SAFETY: attributes is correctly initialized and points to a valid descriptor
    // owned by descriptor. Both remain alive throughout the synchronous create call;
    // CreateNamedPipeW copies the descriptor rather than retaining our allocation.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first_instance)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, ptr::from_mut(&mut attributes).cast())
    }
}

/// Verifies the actual descriptor of an already-connected managed pipe client.
///
/// Requires the process token user as owner, a protected non-null DACL, and exactly
/// one allow ACE with no inheritance flags for that same SID. The mask must equal
/// `FILE_GENERIC_READ | FILE_GENERIC_WRITE`, the factory's mapped duplex rights;
/// broader permissions, including full control, are rejected.
///
/// Uses read-only `GetSecurityInfo` on the supplied handle. It does not open/connect
/// an endpoint, create another pipe instance, change an ACL, or close the client.
/// This is a descriptor snapshot, not server identity authentication or evidence
/// of the remote-client rejection flag. Callers must retain their lifecycle and
/// service identity checks; the SID is not an authentication secret.
///
/// # Errors
///
/// Returns native I/O errors if the descriptor or process user cannot be queried,
/// `InvalidData` for invalid native output, or `PermissionDenied` for any policy
/// mismatch (including the legacy default DACL). There is no permissive fallback.
pub fn verify_private_pipe_client(client: &NamedPipeClient) -> io::Result<()> {
    let mut descriptor = ptr::null_mut();
    // SAFETY: client owns the borrowed live pipe handle and descriptor is writable.
    // SE_KERNEL_OBJECT supports named pipe handles. Success returns a LocalAlloc
    // copy of the requested owner/DACL; no pointers into it escape this call.
    let status = unsafe {
        GetSecurityInfo(
            client.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    let descriptor = LocalAllocation::from_success(descriptor)?;
    // SAFETY: GetSecurityInfo returned a complete live security descriptor.
    if unsafe { IsValidSecurityDescriptor(descriptor.0.as_ptr()) } == 0 {
        return Err(invalid_native_data("invalid pipe security descriptor"));
    }
    let sid: Vec<u16> = current_user_sid()?.encode_utf16().chain(Some(0)).collect();
    let mut native_sid = ptr::null_mut();
    // SAFETY: sid is a live NUL-terminated canonical SID and native_sid is writable.
    // Success returns a complete valid SID with LocalAlloc ownership.
    if unsafe { ConvertStringSidToSidW(sid.as_ptr(), &raw mut native_sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user = LocalAllocation::from_success(native_sid)?;
    verify_private_descriptor(&descriptor, &user)
}

// Both arguments own validated native allocations: a GetSecurityInfo descriptor
// and a converted TokenUser SID. All returned interior pointers remain borrowed.
fn verify_private_descriptor(
    descriptor: &LocalAllocation,
    user: &LocalAllocation,
) -> io::Result<()> {
    let mut owner = ptr::null_mut();
    let mut defaulted = 0;
    // SAFETY: descriptor is valid and live; owner/defaulted are writable outputs.
    if unsafe {
        GetSecurityDescriptorOwner(descriptor.0.as_ptr(), &raw mut owner, &raw mut defaulted)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a non-null owner is a complete SID in the validated descriptor;
    // user owns a valid converted SID. The checks short-circuit before EqualSid.
    let owner_matches = !owner.is_null()
        && unsafe { IsValidSid(owner) != 0 && EqualSid(owner, user.0.as_ptr()) != 0 };
    if !owner_matches {
        return Err(policy_mismatch("pipe owner is not the process token user"));
    }
    let dacl = protected_dacl(descriptor)?;
    // SAFETY: protected_dacl returned a valid, live ACL owned by descriptor.
    if unsafe { (*dacl).AceCount } != 1 {
        return Err(policy_mismatch(
            "pipe DACL must contain exactly one allow ACE",
        ));
    }
    let mut ace = ptr::null_mut();
    // SAFETY: dacl is valid with exactly one ACE and ace is a writable output.
    if unsafe { GetAce(dacl, 0, &raw mut ace) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if ace.is_null() {
        return Err(invalid_native_data("native ACL query returned null ACE"));
    }
    // SAFETY: GetAce returned a live DWORD-aligned ACE with an initialized header.
    let header = unsafe { ace.cast::<ACE_HEADER>().read() };
    // SAFETY: user owns a complete valid SID returned by ConvertStringSidToSidW.
    let sid_length = unsafe { GetLengthSid(user.0.as_ptr()) };
    let sid_length =
        usize::try_from(sid_length).map_err(|_| invalid_native_data("user SID length overflow"))?;
    if header.AceType != 0
        || header.AceFlags != 0
        || usize::from(header.AceSize) != offset_of!(ACCESS_ALLOWED_ACE, SidStart) + sid_length
        || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Err(policy_mismatch(
            "pipe ACE must be a non-inherited user allow ACE",
        ));
    }
    // SAFETY: the verified basic allow type and full SID-sized ACE allow reading
    // ACCESS_ALLOWED_ACE. Its trailing SID remains inside the live descriptor.
    let (mask, sid) = unsafe {
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        (
            (*allowed).Mask,
            ptr::addr_of_mut!((*allowed).SidStart).cast(),
        )
    };
    if mask != FILE_GENERIC_READ | FILE_GENERIC_WRITE {
        return Err(policy_mismatch(
            "pipe ACE must grant exactly mapped read/write rights",
        ));
    }
    // SAFETY: sid has a complete SID-sized backing span inside the live ACE. Test
    // validity and its actual length before EqualSid can read variable contents.
    let sid_matches = unsafe {
        IsValidSid(sid) != 0
            && usize::try_from(GetLengthSid(sid)).ok() == Some(sid_length)
            && EqualSid(sid, user.0.as_ptr()) != 0
    };
    if !sid_matches {
        return Err(policy_mismatch(
            "pipe ACE SID is not the process token user",
        ));
    }
    Ok(())
}

fn protected_dacl(descriptor: &LocalAllocation) -> io::Result<*mut ACL> {
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: descriptor is valid and live; both scalar outputs are writable.
    if unsafe {
        GetSecurityDescriptorControl(descriptor.0.as_ptr(), &raw mut control, &raw mut revision)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let required = SE_DACL_PRESENT | SE_DACL_PROTECTED;
    if control & required != required {
        return Err(policy_mismatch("pipe DACL must be present and protected"));
    }
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = ptr::null_mut();
    // SAFETY: descriptor is valid and live; the outputs receive flags and a
    // borrowed ACL whose allocation remains owned by descriptor.
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0.as_ptr(),
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the null check precedes IsValidAcl; a non-null DACL points into
    // the validated, live descriptor allocation returned by GetSecurityInfo.
    if present == 0 || dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(policy_mismatch("pipe DACL must be non-null and valid"));
    }
    Ok(dacl)
}

fn policy_mismatch(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn validate_name(name: &str) -> io::Result<()> {
    let local_prefix = name
        .get(..PIPE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PIPE_PREFIX));
    if !local_prefix
        || name.len() == PIPE_PREFIX.len()
        || name[PIPE_PREFIX.len()..].contains(['\0', '\\'])
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a nonempty local name: \\\\.\\pipe\\name (no NUL or backslash)",
        ));
    }
    Ok(())
}

/// Returns the current process token user's SID in canonical Windows string form.
///
/// This identifier is suitable for a per-user namespace, not an authentication
/// secret. A thread impersonation token and the token's default owner are not used.
///
/// # Errors
///
/// Returns an error if querying the process token or converting its user SID fails.
/// No account name, environment variable, group SID or default owner is a fallback.
pub fn current_user_sid() -> io::Result<String> {
    let token = ProcessToken::open()?;
    let mut needed = 0;
    // SAFETY: token is live, needed is writable, and NULL/0 is the documented
    // size-query form of GetTokenInformation.
    let probe = unsafe {
        GetTokenInformation(
            token.0.as_ptr(),
            TokenUser,
            ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    let error = io::Error::last_os_error();
    if probe != 0 {
        return Err(invalid_native_data(
            "unexpected successful token size query",
        ));
    }
    if error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
        != Some(ERROR_INSUFFICIENT_BUFFER)
    {
        return Err(error);
    }
    let byte_count =
        usize::try_from(needed).map_err(|_| invalid_native_data("token buffer size overflow"))?;
    if byte_count < size_of::<TOKEN_USER>() {
        return Err(invalid_native_data("token user buffer is too small"));
    }
    // usize backing storage is pointer-aligned (unlike Vec<u8>) and includes the
    // variable-length SID. Pass the original byte count, not rounded capacity.
    let word_count = byte_count.div_ceil(size_of::<usize>());
    let mut words = Vec::<usize>::new();
    words
        .try_reserve_exact(word_count)
        .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
    words.resize(word_count, 0);
    let mut returned = 0;
    // SAFETY: words is initialized, sufficiently sized and TOKEN_USER-aligned;
    // token remains live and returned is writable.
    if unsafe {
        GetTokenInformation(
            token.0.as_ptr(),
            TokenUser,
            words.as_mut_ptr().cast(),
            needed,
            &raw mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if returned > needed || usize::try_from(returned).unwrap_or(0) < size_of::<TOKEN_USER>() {
        return Err(invalid_native_data("invalid returned token user size"));
    }
    // SAFETY: the successful query initialized a TOKEN_USER at this aligned
    // address. Its SID points inside words, which stays live through conversion.
    let user = unsafe { words.as_ptr().cast::<TOKEN_USER>().read() };
    if user.User.Sid.is_null() {
        return Err(invalid_native_data("token user SID is null"));
    }
    // SAFETY: GetTokenInformation returned this live SID in the owned buffer.
    if unsafe { IsValidSid(user.User.Sid) } == 0 {
        return Err(invalid_native_data("token user SID is invalid"));
    }
    let mut string_sid = ptr::null_mut();
    // SAFETY: the validated SID remains live; string_sid is a writable output.
    // Success returns a NUL-terminated UTF-16 string allocated with LocalAlloc.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut string_sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocation = LocalAllocation::from_success(string_sid.cast())?;
    let string_sid = allocation.0.as_ptr().cast::<u16>();
    let mut length = 0;
    // SAFETY: the successful conversion guarantees a terminated UTF-16 string;
    // allocation owns it and no pointer is read past the first terminator.
    while unsafe { string_sid.add(length).read() } != 0 {
        length += 1;
    }
    // SAFETY: length spans precisely the initialized UTF-16 contents of allocation.
    let contents = unsafe { std::slice::from_raw_parts(string_sid, length) };
    String::from_utf16(contents).map_err(|_| invalid_native_data("invalid UTF-16 SID"))
}

fn invalid_native_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct ProcessToken(NonNull<c_void>);

impl ProcessToken {
    fn open() -> io::Result<Self> {
        let mut token = ptr::null_mut();
        // SAFETY: GetCurrentProcess returns the current process pseudo-handle;
        // token is writable. OpenProcessToken success transfers one real handle.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if token == INVALID_HANDLE_VALUE {
            return Err(invalid_native_data("invalid process token handle"));
        }
        NonNull::new(token)
            .map(Self)
            .ok_or_else(|| invalid_native_data("null process token handle"))
    }
}

impl Drop for ProcessToken {
    fn drop(&mut self) {
        // SAFETY: this uniquely owned handle came from OpenProcessToken and has
        // not been closed or exposed to callers. The process pseudo-handle is not owned.
        unsafe { CloseHandle(self.0.as_ptr()) };
    }
}

struct LocalAllocation(NonNull<c_void>);

impl LocalAllocation {
    fn from_success(pointer: *mut c_void) -> io::Result<Self> {
        NonNull::new(pointer)
            .map(Self)
            .ok_or_else(|| invalid_native_data("native conversion returned null"))
    }
}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: both conversion APIs transfer unique LocalAlloc allocations;
        // no borrowed pointers escape the owning allocation's scope.
        unsafe { LocalFree(self.0.as_ptr()) };
    }
}
