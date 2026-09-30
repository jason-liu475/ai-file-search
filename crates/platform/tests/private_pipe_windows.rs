#![cfg(windows)]

//! These tests inspect real pipe descriptors and current-user access. Descriptor
//! evidence is NOT a native connection-denial run under a foreign OS user.

use std::ffi::c_void;
use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ai_file_search_platform::{create_private_pipe, current_user_sid, verify_private_pipe_client};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use tokio::time::timeout;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, GetHandleInformation,
    HANDLE_FLAG_INHERIT, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetLengthSid,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation,
    OWNER_SECURITY_INFORMATION, SE_DACL_PRESENT, SE_DACL_PROTECTED, SE_SELF_RELATIVE,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
use windows_sys::Win32::System::Pipes::GetNamedPipeHandleStateW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

static NEXT_NAME: AtomicU64 = AtomicU64::new(0);
const _: () = assert!(align_of::<usize>() >= align_of::<TOKEN_USER>());

fn pipe_name(label: &str) -> String {
    format!(
        r"\\.\pipe\aifs-platform-{label}-{}-{}",
        std::process::id(),
        NEXT_NAME.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug, Eq, PartialEq)]
struct Policy {
    control: u16,
    owner: Vec<u8>,
    null_dacl: bool,
    aces: Vec<(u8, u8, u32, Vec<u8>)>,
}

struct LocalAllocation(NonNull<c_void>);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: the security APIs allocated this uniquely owned buffer with
        // LocalAlloc; all borrowed pointers are confined to its owner's scope.
        unsafe { LocalFree(self.0.as_ptr()) };
    }
}

fn read_policy(server: &NamedPipeServer) -> Policy {
    let mut descriptor = ptr::null_mut();
    let mut owner = ptr::null_mut();
    // SAFETY: server's handle remains live, the outputs are writable, and
    // SE_KERNEL_OBJECT is the documented type for named pipes. No ACL is changed.
    let result = unsafe {
        GetSecurityInfo(
            server.as_raw_handle(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    assert_eq!(result, ERROR_SUCCESS, "GetSecurityInfo failed: {result}");
    let descriptor = LocalAllocation(NonNull::new(descriptor).expect("non-null descriptor"));
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: descriptor is valid and live; both output scalars are writable.
    assert_ne!(
        unsafe {
            GetSecurityDescriptorControl(descriptor.0.as_ptr(), &raw mut control, &raw mut revision)
        },
        0
    );
    assert_eq!(revision, 1);
    assert_ne!(control & SE_SELF_RELATIVE, 0);
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = ptr::null_mut::<ACL>();
    // SAFETY: the descriptor is valid and live; outputs receive a borrowed DACL
    // and flags. The owning descriptor outlives all following reads.
    assert_ne!(
        unsafe {
            GetSecurityDescriptorDacl(
                descriptor.0.as_ptr(),
                &raw mut present,
                &raw mut dacl,
                &raw mut defaulted,
            )
        },
        0
    );
    assert_ne!(present, 0, "DACL must be present");
    let ace_count = if dacl.is_null() {
        0
    } else {
        // SAFETY: the successful query returned a non-null ACL owned by descriptor.
        unsafe { (*dacl).AceCount }
    };
    let mut aces = Vec::new();
    for index in 0..u32::from(ace_count) {
        let mut ace = ptr::null_mut();
        // SAFETY: dacl is valid, index is in bounds, and ace is a writable output.
        assert_ne!(unsafe { GetAce(dacl, index, &raw mut ace) }, 0);
        assert!(!ace.is_null());
        // SAFETY: GetAce returned a live, aligned ACE whose header is initialized.
        let header = unsafe { ace.cast::<ACE_HEADER>().read() };
        assert!(
            matches!(header.AceType, 0 | 1),
            "expected a basic allow/deny ACE"
        );
        assert!(usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>());
        // SAFETY: basic allow/deny ACEs have this same mask/SID layout and the
        // verified size permits reading it. SidStart begins the complete live SID.
        let (mask, sid) = unsafe {
            let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
            (
                (*allowed).Mask,
                ptr::addr_of_mut!((*allowed).SidStart).cast(),
            )
        };
        // SAFETY: this SID is part of a valid OS-returned ACE and descriptor
        // remains live. sid_bytes copies it before descriptor is released.
        let sid = unsafe { sid_bytes(sid) };
        assert_eq!(
            usize::from(header.AceSize),
            offset_of!(ACCESS_ALLOWED_ACE, SidStart) + sid.len()
        );
        aces.push((header.AceType, header.AceFlags, mask, sid));
    }
    assert!(!owner.is_null());
    // SAFETY: GetSecurityInfo returned a valid owner SID inside live descriptor.
    let owner = unsafe { sid_bytes(owner) };
    Policy {
        control,
        owner,
        null_dacl: dacl.is_null(),
        aces,
    }
}

/// Copies a valid SID while its backing token/descriptor remains alive.
///
/// # Safety
/// `sid` must point to a complete, valid, readable Windows SID for this call.
unsafe fn sid_bytes(sid: *mut c_void) -> Vec<u8> {
    // SAFETY: the caller guarantees a complete, live SID.
    let length = usize::try_from(unsafe { GetLengthSid(sid) }).unwrap();
    // SAFETY: GetLengthSid supplies the size of the caller's valid SID.
    unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), length) }.to_vec()
}

fn process_user_sid() -> Vec<u8> {
    let mut token = ptr::null_mut();
    // SAFETY: the pseudo-handle is valid and token is writable; success returns
    // a new real token handle which is immediately placed into OwnedHandle.
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) },
        0
    );
    assert!(!token.is_null());
    // SAFETY: OpenProcessToken transferred a valid uniquely owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0;
    // SAFETY: NULL/0 queries size and needed is writable; the handle is live.
    assert_eq!(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &raw mut needed,
            )
        },
        0
    );
    assert_eq!(
        io::Error::last_os_error()
            .raw_os_error()
            .and_then(|code| u32::try_from(code).ok()),
        Some(ERROR_INSUFFICIENT_BUFFER)
    );
    let bytes = usize::try_from(needed).unwrap();
    assert!(bytes >= size_of::<TOKEN_USER>());
    let mut buffer = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
    let mut returned = 0;
    // SAFETY: buffer has enough initialized bytes with TOKEN_USER alignment;
    // handle is live and returned is writable.
    assert_ne!(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &raw mut returned,
            )
        },
        0
    );
    assert!(returned <= needed);
    assert!(usize::try_from(returned).unwrap() >= size_of::<TOKEN_USER>());
    // SAFETY: the successful query initialized TOKEN_USER in aligned buffer;
    // the returned SID remains live until sid_bytes has copied its contents.
    unsafe { sid_bytes((*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid) }
}

fn assert_private(policy: &Policy) {
    assert!(!policy.null_dacl, "a NULL DACL grants everyone access");
    assert_ne!(
        policy.control & SE_DACL_PROTECTED,
        0,
        "DACL must be protected"
    );
    assert_ne!(policy.control & SE_DACL_PRESENT, 0);
    let user = process_user_sid();
    assert_eq!(
        policy.owner, user,
        "owner must be TokenUser, not TokenOwner"
    );
    assert_eq!(
        policy.aces,
        vec![(0, 0, FILE_GENERIC_READ | FILE_GENERIC_WRITE, user)],
        "exactly one non-inherited user allow ACE with only duplex rights"
    );
}

#[test]
fn safe_sid_api_matches_process_token_user_and_is_stable() {
    let sid = current_user_sid().unwrap();
    assert_eq!(current_user_sid().unwrap(), sid);
    assert!(sid.starts_with("S-1-"));
    assert!(
        sid.bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'S' | b'-'))
    );
    let wide: Vec<u16> = sid.encode_utf16().chain(Some(0)).collect();
    let mut native_sid = ptr::null_mut();
    // SAFETY: wide is terminated and live, and native_sid is writable. Success
    // returns a valid SID with LocalAlloc ownership.
    assert_ne!(
        unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut native_sid) },
        0
    );
    let native_sid = LocalAllocation(NonNull::new(native_sid).unwrap());
    // SAFETY: conversion returned a valid SID owned by native_sid until copied.
    assert_eq!(
        unsafe { sid_bytes(native_sid.0.as_ptr()) },
        process_user_sid()
    );
}

#[tokio::test]
async fn first_instance_has_exact_protected_user_dacl_and_noninheritable_handle() {
    let server = create_private_pipe(&pipe_name("descriptor"), true).unwrap();
    assert_private(&read_policy(&server));
    let mut flags = 0;
    // SAFETY: server owns the live handle and flags is a writable output.
    assert_ne!(
        unsafe { GetHandleInformation(server.as_raw_handle(), &raw mut flags) },
        0
    );
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
}

#[tokio::test]
async fn current_user_client_connects_and_exchanges_bytes() {
    timeout(Duration::from_secs(5), async {
        let name = pipe_name("exchange");
        let server = create_private_pipe(&name, true).unwrap();
        let mut client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let mut server = server;
        client.write_all(b"ping").await.unwrap();
        let mut bytes = [0; 4];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
        server.write_all(b"pong").await.unwrap();
        client.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"pong");
    })
    .await
    .expect("current-user pipe I/O must finish");
}

#[tokio::test]
async fn subsequent_instances_preserve_exact_private_policy() {
    let name = pipe_name("subsequent");
    let first = create_private_pipe(&name, true).unwrap();
    let expected = read_policy(&first);
    assert_private(&expected);
    let next = create_private_pipe(&name, false).unwrap();
    assert_eq!(read_policy(&next), expected);
    drop(first);
    let third = create_private_pipe(&name, false).unwrap();
    assert_eq!(read_policy(&third), expected);
    assert_private(&read_policy(&next));
}

#[tokio::test]
async fn nonfirst_flag_also_applies_dacl_when_creating_a_new_pipe() {
    // Independent creation proves false does not skip the security attributes;
    // live instances otherwise share the named pipe's security descriptor.
    let server = create_private_pipe(&pipe_name("nonfirst-new"), false).unwrap();
    assert_private(&read_policy(&server));
}

#[tokio::test]
async fn first_instance_collision_is_access_denied_and_keeps_owned_pipe() {
    let name = pipe_name("collision");
    let existing = create_private_pipe(&name, true).unwrap();
    let policy = read_policy(&existing);
    let error = create_private_pipe(&name, true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        error
            .raw_os_error()
            .and_then(|code| u32::try_from(code).ok()),
        Some(ERROR_ACCESS_DENIED)
    );
    assert_eq!(read_policy(&existing), policy);
    assert!(ClientOptions::new().open(&name).is_ok());
}

#[tokio::test]
async fn first_instance_collision_never_rewrites_a_preexisting_default_dacl() {
    let name = pipe_name("foreign-existing");
    let existing = ServerOptions::new().create(&name).unwrap();
    let before = read_policy(&existing);
    let error = create_private_pipe(&name, true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(read_policy(&existing), before);
}

#[tokio::test]
async fn verify_secure_client_succeeds_without_changing_descriptor_or_connection() {
    timeout(Duration::from_secs(5), async {
        let name = pipe_name("verify-secure");
        let mut server = create_private_pipe(&name, true).unwrap();
        let before = read_policy(&server);
        let mut client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        verify_private_pipe_client(&client).unwrap();
        assert_eq!(read_policy(&server), before);
        assert_eq!(instance_count(&server), 1);
        client.write_all(b"live").await.unwrap();
        let mut bytes = [0; 4];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"live");
    })
    .await
    .expect("verified connection must remain usable");
}

#[tokio::test]
async fn verify_legacy_default_client_is_rejected_without_acl_or_instance_changes() {
    timeout(Duration::from_secs(5), async {
        let name = pipe_name("verify-legacy");
        let mut server = ServerOptions::new().create(&name).unwrap();
        let before = read_policy(&server);
        let mut client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        assert_eq!(instance_count(&server), 1);
        let error = verify_private_pipe_client(&client).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(read_policy(&server), before);
        assert_eq!(instance_count(&server), 1);
        // Rejection is read-only: keep the original client usable for legacy stop.
        client.write_all(b"live").await.unwrap();
        let mut bytes = [0; 4];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"live");
    })
    .await
    .expect("rejected legacy connection must remain usable");
}

fn instance_count(server: &NamedPipeServer) -> u32 {
    let mut count = 0;
    // SAFETY: server owns the live handle, count is writable, and all unrequested
    // optional outputs are NULL. No pipe instance is created by this query.
    assert_ne!(
        unsafe {
            GetNamedPipeHandleStateW(
                server.as_raw_handle(),
                ptr::null_mut(),
                &raw mut count,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
            )
        },
        0
    );
    count
}

fn pipe_with_sddl(name: &str, sddl: &str) -> NamedPipeServer {
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: wide is terminated and live; descriptor is writable. Success returns
    // a complete LocalAlloc descriptor owned by the RAII allocation below.
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        },
        0
    );
    let descriptor = LocalAllocation(NonNull::new(descriptor).unwrap());
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: descriptor.0.as_ptr(),
        bInheritHandle: 0,
    };
    // SAFETY: attributes and its valid descriptor stay live throughout synchronous
    // creation. This applies only to our uniquely named temporary test pipe.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, ptr::from_mut(&mut attributes).cast())
    }
    .unwrap()
}

#[tokio::test]
async fn verify_rejects_native_policy_variants_without_changing_acl_or_instances() {
    timeout(Duration::from_secs(5), async {
        let user = current_user_sid().unwrap();
        for sddl in [
            format!("O:{user}D:(A;;GRGW;;;{user})"),
            format!("O:{user}D:NO_ACCESS_CONTROL"),
            format!("O:{user}D:P(A;;GRGW;;;{user})(A;;GR;;;WD)"),
            format!("O:{user}D:P(A;ID;GRGW;;;{user})"),
            format!("O:{user}D:P(A;;GA;;;{user})"),
            format!("O:{user}D:P(A;;GRGW;;;WD)"),
        ] {
            let name = pipe_name("verify-policy-variant");
            let server = pipe_with_sddl(&name, &sddl);
            let before = read_policy(&server);
            let client = ClientOptions::new().open(&name).unwrap();
            server.connect().await.unwrap();
            let error = verify_private_pipe_client(&client).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{sddl}");
            assert_eq!(read_policy(&server), before, "{sddl}");
            assert_eq!(instance_count(&server), 1, "{sddl}");
        }
    })
    .await
    .expect("policy-variant verification must finish");
}

#[tokio::test]
async fn verify_without_read_control_fails_closed_without_changing_pipe() {
    let name = pipe_name("verify-query-denied");
    let server = create_private_pipe(&name, true).unwrap();
    let before = read_policy(&server);
    let client = ClientOptions::new()
        .read(false)
        .write(false)
        .open(&name)
        .unwrap();
    timeout(Duration::from_secs(5), server.connect())
        .await
        .unwrap()
        .unwrap();
    let error = verify_private_pipe_client(&client).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        error.raw_os_error(),
        Some(5),
        "must propagate GetSecurityInfo failure"
    );
    assert_eq!(read_policy(&server), before);
    assert_eq!(instance_count(&server), 1);
}

#[tokio::test]
async fn custom_names_keep_case_unicode_and_punctuation() {
    let name = format!("{}-\u{6587}\u{4ef6} @+ .", pipe_name("custom"));
    let server = create_private_pipe(&name.to_uppercase(), true).unwrap();
    assert_private(&read_policy(&server));
    let _client = ClientOptions::new().open(&name).unwrap();
    timeout(Duration::from_secs(5), server.connect())
        .await
        .expect("custom-name client must connect")
        .unwrap();
}

#[tokio::test]
async fn invalid_names_fail_before_creating_a_truncated_or_nonlocal_pipe() {
    for name in [
        "",
        "bare-name",
        r"\\.\pipe\",
        r"\\remote\pipe\name",
        r"\\.\pipe\nested\name",
    ] {
        let error = create_private_pipe(name, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
    let name = pipe_name("nul");
    let error = create_private_pipe(&format!("{name}\0ignored"), true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let server = create_private_pipe(&name, true).unwrap();
    assert_private(&read_policy(&server));
}
