use super::{LocalAllocation, current_user_sid, verify_private_descriptor};
use std::io;
use std::ptr;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, SDDL_REVISION_1,
};

#[test]
fn native_descriptor_owner_and_unusable_dacl_variants_are_rejected() {
    let user = current_user_sid().unwrap();
    let wide: Vec<u16> = user.encode_utf16().chain(Some(0)).collect();
    let mut sid = ptr::null_mut();
    // SAFETY: wide is terminated and live; sid is writable. Success transfers
    // one complete SID allocated with LocalAlloc to the RAII owner below.
    assert_ne!(
        unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut sid) },
        0
    );
    let sid = LocalAllocation::from_success(sid).unwrap();
    for sddl in [
        format!("O:WDD:P(A;;GRGW;;;{user})"),
        format!("D:P(A;;GRGW;;;{user})"),
        format!("O:{user}D:P"),
        format!("O:{user}"),
        format!("O:{user}D:P(A;IO;GRGW;;;{user})"),
    ] {
        let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = ptr::null_mut();
        // SAFETY: wide is terminated and live; descriptor is writable. Success
        // returns a valid LocalAlloc descriptor, without applying it to an object.
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
        let descriptor = LocalAllocation::from_success(descriptor).unwrap();
        let error = verify_private_descriptor(&descriptor, &sid).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{sddl}");
    }
}
