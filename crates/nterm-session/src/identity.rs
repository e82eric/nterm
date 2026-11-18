//! Who a process is, for deciding whether the other end of the pipe may be
//! talked to: both ends must run as the same account at the same elevation.
//!
//! Not as the same sign-in. A user who signs in twice, at the desktop and
//! over SSH say, is one user with one set of sessions, and a relay started
//! from one sign-in must reach the daemon of another. So what differs between
//! two sign-ins of an account is not compared: the logon session, the
//! desktop session, and the groups that say how the user signed in.
//!
//! What this gives up: a program started as the same account with some of
//! its groups switched off, but at the same integrity level, was refused
//! when groups were compared and is not now. Restricted and AppContainer
//! tokens, which are how programs are usually confined, are still refused.
use std::{io, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::ConvertSidToStringSidW,
    Security::*,
    System::Threading::*,
};

pub(crate) struct Handle(pub HANDLE);
// A kernel handle may be used from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(crate) fn owned(handle: HANDLE) -> io::Result<Handle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(handle))
    }
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.to_owned())
}

// Token structures hold pointers, so the buffer is aligned for them.
struct TokenData(Vec<u64>);

fn token_data(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<TokenData> {
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token, class, null_mut(), 0, &mut bytes);
    }
    if bytes == 0 || bytes > 65536 {
        return Err(denied("unable to inspect token"));
    }
    let mut data = TokenData(vec![0; (bytes as usize).div_ceil(8)]);
    if unsafe { GetTokenInformation(token, class, data.0.as_mut_ptr().cast(), bytes, &mut bytes) }
        == 0
    {
        return Err(denied("token inspection failed"));
    }
    Ok(data)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(denied("unable to inspect SID"));
    }
    let mut len = 0;
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
    }
    let result = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    unsafe {
        LocalFree(text.cast());
    }
    Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    /// The account's SID.
    pub user: String,
    /// The integrity level: 0x2000 for an ordinary program, 0x3000 for an
    /// elevated one.
    pub integrity: u32,
    elevated: bool,
}

pub(crate) fn identity(token: HANDLE) -> io::Result<Identity> {
    if unsafe { IsTokenRestricted(token) } != 0 {
        return Err(denied("restricted tokens are not supported"));
    }
    if token_data(token, TokenIsAppContainer)?.0[0] as u32 != 0 {
        return Err(denied("AppContainer tokens are not supported"));
    }
    let user = token_data(token, TokenUser)?;
    let user = sid_string(unsafe { (*(user.0.as_ptr().cast::<TOKEN_USER>())).User.Sid })?;
    let elevated = token_data(token, TokenElevation)?.0[0] as u32 != 0;
    let label = token_data(token, TokenIntegrityLevel)?;
    let sid = unsafe { (*(label.0.as_ptr().cast::<TOKEN_MANDATORY_LABEL>())).Label.Sid };
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return Err(denied("invalid integrity SID"));
    }
    let integrity = unsafe { *GetSidSubAuthority(sid, count as u32 - 1) };
    Ok(Identity { user, integrity, elevated })
}

pub(crate) fn process_token(process: HANDLE) -> io::Result<Handle> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(denied("unable to query process token"));
    }
    owned(token)
}

pub(crate) fn own_identity() -> io::Result<Identity> {
    identity(process_token(unsafe { GetCurrentProcess() })?.0)
}

/// The identity of another process, which must still be running.
pub(crate) fn process_identity(pid: u32) -> io::Result<Identity> {
    let process =
        owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) })
            .map_err(|_| denied("unable to open the peer process"))?;
    if unsafe { WaitForSingleObject(process.0, 0) } != WAIT_TIMEOUT {
        return Err(denied("the peer process has exited"));
    }
    identity(process_token(process.0)?.0)
}


/// The identity a pipe client is acting with, taken by impersonating it.
pub(crate) fn impersonated(
    impersonate: impl FnOnce() -> bool,
) -> io::Result<Identity> {
    if !impersonate() {
        return Err(denied("client impersonation failed"));
    }
    let result = (|| {
        let mut token = null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
            return Err(denied("unable to inspect the client token"));
        }
        let token = owned(token)?;
        let level = token_data(token.0, TokenImpersonationLevel)?.0[0] as u32;
        if level < SecurityIdentification as u32 {
            return Err(denied("client identification is unavailable"));
        }
        identity(token.0)
    })();
    // Carrying on as the client would be unsafe.
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_is_its_account_and_its_elevation() {
        let own = own_identity().unwrap();
        // An account of this machine or its domain, not a sign-in ("S-1-5-5-").
        assert!(own.user.starts_with("S-1-5-21-"), "{own:?}");
        assert!(own.integrity >= 0x2000, "{own:?}");
        assert_eq!(own.elevated, own.integrity >= 0x3000, "{own:?}");
        assert_eq!(process_identity(std::process::id()).unwrap(), own);
    }

    #[test]
    fn another_account_or_elevation_is_another_identity() {
        let own = own_identity().unwrap();
        let other = Identity { user: "S-1-5-21-1-2-3-1001".into(), ..own.clone() };
        assert_ne!(own, other);
        let raised = Identity { integrity: 0x3000, elevated: true, ..own.clone() };
        assert_eq!(own == raised, own.elevated);
    }
}
