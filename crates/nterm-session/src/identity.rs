//! Who a process is, for deciding whether the other end of the pipe may be
//! talked to. The comparison is the one the picker pipe makes: both ends must
//! run with the same token in every respect that grants access.
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
    user: String,
    logon: (u32, i32),
    session: u32,
    pub integrity: u32,
    elevated: bool,
    elevation_type: u32,
    groups: Vec<(String, u32)>,
    privileges: Vec<(u32, i32, u32)>,
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
    let stats = token_data(token, TokenStatistics)?;
    let stats = unsafe { &*stats.0.as_ptr().cast::<TOKEN_STATISTICS>() };
    let session = token_data(token, TokenSessionId)?.0[0] as u32;
    let elevated = token_data(token, TokenElevation)?.0[0] as u32 != 0;
    let elevation_type = token_data(token, TokenElevationType)?.0[0] as u32;
    let group_data = token_data(token, TokenGroups)?;
    let group_data = unsafe { &*group_data.0.as_ptr().cast::<TOKEN_GROUPS>() };
    let group_entries = unsafe {
        std::slice::from_raw_parts(group_data.Groups.as_ptr(), group_data.GroupCount as usize)
    };
    let mut groups = group_entries
        .iter()
        .map(|g| sid_string(g.Sid).map(|sid| (sid, g.Attributes)))
        .collect::<io::Result<Vec<_>>>()?;
    groups.sort_unstable();
    let privilege_data = token_data(token, TokenPrivileges)?;
    let privilege_data = unsafe { &*privilege_data.0.as_ptr().cast::<TOKEN_PRIVILEGES>() };
    let entries = unsafe {
        std::slice::from_raw_parts(
            privilege_data.Privileges.as_ptr(),
            privilege_data.PrivilegeCount as usize,
        )
    };
    // Which privileges are held, not whether they are enabled: a process may
    // toggle the ones it holds, so only SE_PRIVILEGE_REMOVED (0x4) matters.
    let mut privileges: Vec<_> = entries
        .iter()
        .map(|p| (p.Luid.LowPart, p.Luid.HighPart, p.Attributes & 0x4))
        .collect();
    privileges.sort_unstable();
    let label = token_data(token, TokenIntegrityLevel)?;
    let sid = unsafe { (*(label.0.as_ptr().cast::<TOKEN_MANDATORY_LABEL>())).Label.Sid };
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return Err(denied("invalid integrity SID"));
    }
    let integrity = unsafe { *GetSidSubAuthority(sid, count as u32 - 1) };
    Ok(Identity {
        user,
        logon: (stats.AuthenticationId.LowPart, stats.AuthenticationId.HighPart),
        session,
        integrity,
        elevated,
        elevation_type,
        groups,
        privileges,
    })
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

/// The SID of this logon session, which names and guards the pipe.
pub(crate) fn logon_sid() -> io::Result<String> {
    let token = process_token(unsafe { GetCurrentProcess() })?;
    let data = token_data(token.0, TokenGroups)?;
    let groups = unsafe { &*data.0.as_ptr().cast::<TOKEN_GROUPS>() };
    let entries =
        unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize) };
    entries
        .iter()
        .find(|g| g.Attributes & 0xC0000000 == 0xC0000000)
        .ok_or_else(|| denied("missing logon SID"))
        .and_then(|g| sid_string(g.Sid))
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
