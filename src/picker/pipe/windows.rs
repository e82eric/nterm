//! Windows transport. Handles are never inherited or delegated.
use super::*;
use std::{
    collections::HashSet,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::Mutex,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::*,
    Security::*,
    Storage::FileSystem::*,
    System::{Diagnostics::ToolHelp::*, IO::*, Pipes::*, Threading::*},
};

struct Handle(HANDLE);
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn owned(handle: HANDLE) -> Result<Handle, String> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(Handle(handle))
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

struct TokenData(Vec<u64>);
fn token_data(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<TokenData, String> {
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token, class, null_mut(), 0, &mut bytes);
    }
    if bytes == 0 || bytes > 65536 {
        return Err("unable to inspect client token".into());
    }
    let mut data = TokenData(vec![0; (bytes as usize).div_ceil(8)]);
    if unsafe { GetTokenInformation(token, class, data.0.as_mut_ptr().cast(), bytes, &mut bytes) }
        == 0
    {
        return Err("token inspection failed".into());
    }
    Ok(data)
}
fn sid_string(sid: PSID) -> Result<String, String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err("unable to inspect SID".into());
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
struct Identity {
    user: String,
    logon: (u32, i32),
    session: u32,
    integrity: u32,
    elevated: bool,
    elevation_type: u32,
    groups: Vec<(String, u32)>,
    privileges: Vec<(u32, i32, u32)>,
}
fn identity(token: HANDLE) -> Result<Identity, String> {
    if unsafe { IsTokenRestricted(token) } != 0 {
        return Err("restricted clients are not supported".into());
    }
    let app = token_data(token, TokenIsAppContainer)?;
    if app.0[0] as u32 != 0 {
        return Err("AppContainer clients are not supported".into());
    }
    let user = token_data(token, TokenUser)?;
    let user = sid_string(unsafe { (*(user.0.as_ptr().cast::<TOKEN_USER>())).User.Sid })?;
    let stats = token_data(token, TokenStatistics)?;
    let stats = unsafe { &*stats.0.as_ptr().cast::<TOKEN_STATISTICS>() };
    let session = token_data(token, TokenSessionId)?.0[0] as u32;
    let elevation = token_data(token, TokenElevation)?.0[0] as u32 != 0;
    let elevation_type = token_data(token, TokenElevationType)?.0[0] as u32;
    let group_data = token_data(token, TokenGroups)?;
    let group_data = unsafe { &*group_data.0.as_ptr().cast::<TOKEN_GROUPS>() };
    let group_entries = unsafe {
        std::slice::from_raw_parts(group_data.Groups.as_ptr(), group_data.GroupCount as usize)
    };
    let mut groups = group_entries
        .iter()
        .map(|g| sid_string(g.Sid).map(|sid| (sid, g.Attributes)))
        .collect::<Result<Vec<_>, _>>()?;
    groups.sort_unstable();
    let privilege_data = token_data(token, TokenPrivileges)?;
    let privilege_data = unsafe { &*privilege_data.0.as_ptr().cast::<TOKEN_PRIVILEGES>() };
    let entries = unsafe {
        std::slice::from_raw_parts(
            privilege_data.Privileges.as_ptr(),
            privilege_data.PrivilegeCount as usize,
        )
    };
    let mut privileges: Vec<_> = entries
        .iter()
        .map(|p| (p.Luid.LowPart, p.Luid.HighPart, p.Attributes))
        .collect();
    privileges.sort_unstable();
    let label = token_data(token, TokenIntegrityLevel)?;
    let sid = unsafe {
        (*(label.0.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()))
            .Label
            .Sid
    };
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return Err("invalid integrity SID".into());
    }
    let integrity = unsafe { *GetSidSubAuthority(sid, count as u32 - 1) };
    Ok(Identity {
        user,
        logon: (
            stats.AuthenticationId.LowPart,
            stats.AuthenticationId.HighPart,
        ),
        session,
        integrity,
        elevated: elevation,
        elevation_type,
        groups,
        privileges,
    })
}
fn process_token(process: HANDLE) -> Result<Handle, String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err("unable to query process token".into());
    }
    owned(token)
}
fn logon_sid(token: HANDLE) -> Result<String, String> {
    let data = token_data(token, TokenGroups)?;
    let groups = unsafe { &*data.0.as_ptr().cast::<TOKEN_GROUPS>() };
    let entries =
        unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize) };
    entries
        .iter()
        .find(|g| g.Attributes & 0xC0000000 == 0xC0000000)
        .ok_or("missing logon SID".into())
        .and_then(|g| sid_string(g.Sid))
}
fn process(pid: u32) -> Result<Handle, String> {
    owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) })
}
fn creation(handle: HANDLE) -> Result<u64, String> {
    let (mut start, mut end, mut kernel, mut user) =
        unsafe { (zeroed(), zeroed(), zeroed(), zeroed()) };
    if unsafe { GetProcessTimes(handle, &mut start, &mut end, &mut kernel, &mut user) } == 0 {
        return Err("unable to inspect process lifetime".into());
    }
    Ok((start.dwHighDateTime as u64) << 32 | start.dwLowDateTime as u64)
}
struct Shell {
    handle: Handle,
    pid: u32,
    created: u64,
}
// Only immutable process queries are performed through this retained handle.
unsafe impl Sync for Shell {}
fn ancestry(client: HANDLE, shell: &Shell) -> Result<(), String> {
    if unsafe { WaitForSingleObject(shell.handle.0, 0) } != WAIT_TIMEOUT {
        return Err("pane shell has exited".into());
    }
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut parents = std::collections::HashMap::new();
    let mut ok = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    while ok != 0 {
        parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        ok = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    let mut pid = unsafe { GetProcessId(client) };
    let mut born = creation(client)?;
    let mut seen = HashSet::new();
    for _ in 0..64 {
        if pid == shell.pid {
            return if born == shell.created {
                Ok(())
            } else {
                Err("shell identity changed".into())
            };
        }
        if !seen.insert(pid) {
            break;
        }
        let parent = *parents.get(&pid).ok_or("cannot verify shell ancestry")?;
        let handle = process(parent)?;
        let parent_born = creation(handle.0)?;
        if parent_born > born || unsafe { WaitForSingleObject(handle.0, 0) } != WAIT_TIMEOUT {
            break;
        }
        pid = parent;
        born = parent_born;
    }
    Err("client does not belong to this pane shell".into())
}

struct Shared {
    stop: AtomicBool,
    connected: AtomicBool,
    idle: AtomicBool,
    shell: Mutex<Option<Arc<Shell>>>,
    policy: Mutex<Policy>,
    active: Mutex<Option<(Arc<Session>, Capability)>>,
    identity: Identity,
    context: egui::Context,
    native_wake: Mutex<
        Option<(
            egui_winit::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
            egui_winit::winit::window::WindowId,
        )>,
    >,
}
impl Shared {
    fn wake_ui(&self) {
        if let Some((proxy, window_id)) = self.native_wake.lock().unwrap().as_ref() {
            let _ = proxy.send_event(crate::app::UserEvent::PickerWake {
                window_id: *window_id,
            });
        }
        self.context.request_repaint();
    }
}
enum Capability {
    Named(String),
    Stream,
}
impl Capability {
    fn permitted(&self, policy: &Policy) -> bool {
        policy.enabled
            && match self {
                Self::Named(name) => policy.named.contains(name),
                Self::Stream => policy.allow_stream,
            }
    }
}
fn authorize(pipe: HANDLE, client: HANDLE, shared: &Shared) -> Result<(), String> {
    if identity(process_token(unsafe { GetCurrentProcess() })?.0)? != shared.identity {
        return Err("terminal security context changed".into());
    }
    // Query the effective security context of the message, not just the PID's
    // primary token. Always revert; continuing after failed reversion is unsafe.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err("client impersonation failed".into());
    }
    let result = (|| {
        let mut token = null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
            return Err("unable to inspect effective client token".into());
        }
        let token = owned(token)?;
        let level = token_data(token.0, TokenImpersonationLevel)?.0[0] as u32;
        if level < SecurityIdentification as u32 {
            return Err("client identification is unavailable".into());
        }
        identity(token.0)
    })();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    let effective = result?;
    if effective != shared.identity {
        return Err("client security context differs from terminal".into());
    }
    if unsafe { WaitForSingleObject(client, 0) } != WAIT_TIMEOUT {
        return Err("client has exited".into());
    }
    let primary = identity(process_token(client)?.0)?;
    if primary != effective {
        return Err("effective and primary client identities differ".into());
    }
    let shell = shared
        .shell
        .lock()
        .unwrap()
        .clone()
        .ok_or("shell identity is not available yet")?;
    ancestry(client, &shell)
}

pub(crate) struct Service {
    pub endpoint: String,
    pub token: String,
    pub events: Receiver<Event>,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Service {
    pub fn new(context: egui::Context) -> Result<Self, String> {
        let endpoint = format!(
            "ghostty-picker-{}",
            super::super::wire::new_token().map_err(|e| e.to_string())?
        );
        let token = super::super::wire::new_token().map_err(|e| e.to_string())?;
        let own = process_token(unsafe { GetCurrentProcess() })?;
        let id = identity(own.0)?;
        let logon = logon_sid(own.0)?;
        // Precise client rights: no FILE_CREATE_PIPE_INSTANCE / append right.
        let label = if id.integrity >= 0x3000 { "HI" } else { "ME" };
        let sddl = wide(&format!(
            "D:P(A;;0x00120083;;;{logon})S:(ML;;NWNR;;;{label})"
        ));
        let mut descriptor = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err("unable to secure picker pipe".into());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let pipe = owned(unsafe {
            CreateNamedPipeW(
                wide(&format!(r"\\.\pipe\{endpoint}")).as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                &attributes,
            )
        });
        unsafe {
            LocalFree(descriptor);
        }
        let pipe = pipe?;
        if unsafe { SetHandleInformation(pipe.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err("unable to disable pipe inheritance".into());
        }
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            shell: Mutex::new(None),
            policy: Mutex::new(Policy::default()),
            active: Mutex::new(None),
            identity: id,
            context,
            native_wake: Mutex::new(None),
        });
        let (tx, events) = crossbeam_channel::bounded(2);
        let state = shared.clone();
        let name = endpoint.clone();
        let secret = token.clone();
        let thread = std::thread::Builder::new()
            .name("picker pipe".into())
            .spawn(move || {
                let pipe = pipe;
                while !state.stop.load(Ordering::Acquire) {
                    if connect(pipe.0, &state).is_err() {
                        if state.stop.load(Ordering::Acquire) {
                            break;
                        }
                        continue;
                    }
                    // Failures intentionally do not echo credentials or request data.
                    let _ = serve(pipe.0, &state, &name, &secret, &tx);
                    unsafe {
                        DisconnectNamedPipe(pipe.0);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            endpoint,
            token,
            events,
            shared,
            thread: Some(thread),
        })
    }
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }
    pub fn bind_window(
        &self,
        proxy: egui_winit::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
        window_id: egui_winit::winit::window::WindowId,
    ) {
        *self.shared.native_wake.lock().unwrap() = Some((proxy, window_id));
    }
    pub fn enabled(&self) -> bool {
        self.shared.policy.lock().unwrap().enabled
    }
    pub fn update(&self, policy: &Policy, shell_pid: u32, expected_creation: u64) {
        *self.shared.policy.lock().unwrap() = policy.clone();
        if let Some((session, capability)) = self.shared.active.lock().unwrap().as_ref() {
            if !capability.permitted(policy) {
                session.cancelled.store(true, Ordering::Release);
                session.finish("cancelled", json!({"reason":"permission_revoked"}));
                self.shared.context.request_repaint();
            }
        }
        let mut shell = self.shared.shell.lock().unwrap();
        if shell.is_none() && shell_pid != 0 && expected_creation != 0 {
            if let Ok(handle) = process(shell_pid) {
                if let Ok(created) = creation(handle.0) {
                    if created != expected_creation {
                        return;
                    }
                    if process_token(handle.0)
                        .and_then(|t| identity(t.0))
                        .is_ok_and(|id| id == self.shared.identity)
                    {
                        *shell = Some(Arc::new(Shell {
                            handle,
                            pid: shell_pid,
                            created,
                        }));
                    }
                }
            }
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run_client(service: &Service, command: &str, token: &str) -> std::process::Child {
        run_client_with_initialization(service, command, token, false)
    }
    fn run_client_with_initialization(
        service: &Service,
        command: &str,
        token: &str,
        background: bool,
    ) -> std::process::Child {
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Invoke-GhosttyPicker.ps1");
        std::process::Command::new("pwsh")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "$ErrorActionPreference='Stop'; . '{}' {}; {command}",
                    script.display(),
                    if background {
                        "-BackgroundInitialize"
                    } else {
                        ""
                    }
                ),
            ])
            .env("GHOSTTY_PICKER_ENDPOINT", &service.endpoint)
            .env("GHOSTTY_PICKER_TOKEN", token)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }
    #[test]
    fn powershell_client_streams_batches_receives_json_and_cancels() {
        for cancel in [false, true] {
            let service = Service::new(egui::Context::default()).unwrap();
            service.update(
                &Policy {
                    enabled: true,
                    allow_stream: true,
                    ..Default::default()
                },
                unsafe { GetCurrentProcessId() },
                creation(unsafe { GetCurrentProcess() }).unwrap(),
            );
            let mut child = run_client(
                &service,
                if cancel {
                    "'one' | Invoke-GhosttyPicker -CancelAfterSeconds 1 -PassThruResult | ConvertTo-Json -Depth 16 -Compress"
                } else {
                    "1..20 | ForEach-Object { [pscustomobject]@{ Name=\"item$_\"; Path=\"value$_\" } } | Invoke-GhosttyPicker -DisplayProperty Name -ValueProperty Path -Options @{initial_query=$null;preview_visible=$true;preview=@{type='Text';text='preview'}} -PassThruResult | ConvertTo-Json -Depth 8 -Compress"
                },
                &service.token,
            );
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut collected = Vec::new();
            let mut session = None;
            loop {
                if let Ok(event) = service.events.recv_timeout(Duration::from_millis(50)) {
                    match event {
                        Event::Open { session: s, .. } => {
                            s.reply(json!({"type":"opened","request_id":s.id}));
                            session = Some(s);
                        }
                        Event::Items {
                            items,
                            sequence,
                            session: s,
                        } => {
                            collected.extend(items);
                            s.reply(json!({"type":"ack","request_id":s.id,"sequence":sequence}));
                        }
                        Event::End { session: s } => {
                            s.reply(json!({"type":"input_complete","request_id":s.id}));
                            if !cancel {
                                let item = &collected[19];
                                s.finish(
                                    "accepted",
                                    json!({"selected":[{"id":item.id,"value":item.value}]}),
                                );
                            }
                        }
                    }
                }
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("PowerShell pipe client stalled");
                }
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                value["status"],
                if cancel { "cancelled" } else { "accepted" }
            );
            if !cancel {
                assert_eq!(collected.len(), 20);
                assert_eq!(value["selected"][0]["value"], "value20");
            }
            assert!(session.unwrap().done());
        }
    }
    #[test]
    fn loading_helper_connects_before_first_request_in_foreground_and_background() {
        for background in [false, true] {
            let service = Service::new(egui::Context::default()).unwrap();
            service.update(
                &Policy {
                    enabled: true,
                    allow_stream: true,
                    ..Default::default()
                },
                unsafe { GetCurrentProcessId() },
                creation(unsafe { GetCurrentProcess() }).unwrap(),
            );
            let mut child = run_client_with_initialization(
                &service,
                r#"
                $deadline = [datetime]::UtcNow.AddSeconds(15)
                while ($null -ne $script:GhosttyPickerInitialization -and -not $script:GhosttyPickerInitialization.Pending.IsCompleted) {
                    if ([datetime]::UtcNow -ge $deadline) { throw 'Startup connection stalled' }
                    Start-Sleep -Milliseconds 20
                }
                Start-Sleep -Milliseconds 200
                Initialize-GhosttyPickerClient
                $original = $script:GhosttyPickerConnection.Client
                'warm' | Invoke-GhosttyPicker -PassThruResult | Out-Null
                [object]::ReferenceEquals($original, $script:GhosttyPickerConnection.Client) | ConvertTo-Json -Depth 16 -Compress
            "#,
                &service.token,
                background,
            );
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut saw_connected_before_open = false;
            let mut opened = false;
            loop {
                if !opened && service.connected() && service.shared.active.lock().unwrap().is_none()
                {
                    saw_connected_before_open = true;
                }
                if let Ok(event) = service.events.recv_timeout(Duration::from_millis(10)) {
                    match event {
                        Event::Open { session, .. } => {
                            assert!(
                                saw_connected_before_open,
                                "Connection was not warmed during load (background={background})"
                            );
                            opened = true;
                            session.reply(json!({"type":"opened","request_id":session.id}));
                        }
                        Event::Items {
                            session, sequence, ..
                        } => session.reply(
                            json!({"type":"ack","request_id":session.id,"sequence":sequence}),
                        ),
                        Event::End { session } => {
                            session.finish("accepted", json!({"selected":[]}))
                        }
                    }
                }
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("Startup client stalled");
                }
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(opened);
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                true
            );
        }
    }
    #[test]
    fn persistent_client_reuses_after_accept_cancel_and_denial_then_reconnects() {
        let service = Service::new(egui::Context::default()).unwrap();
        service.update(
            &Policy {
                enabled: true,
                allow_stream: true,
                named: vec!["demo".into()],
                ..Default::default()
            },
            unsafe { GetCurrentProcessId() },
            creation(unsafe { GetCurrentProcess() }).unwrap(),
        );
        let mut child = run_client(
            &service,
            r#"
            $results = [System.Collections.Generic.List[object]]::new()
            $before = if ($null -ne $script:GhosttyPickerConnection) { $script:GhosttyPickerConnection.Client } else { $null }
            $r = 1..20 | Invoke-GhosttyPicker -PassThruResult
            $original = $script:GhosttyPickerConnection.Client
            $results.Add(@{status=$r.status;reused=($null -ne $before -and [object]::ReferenceEquals($before, $script:GhosttyPickerConnection.Client))})
            Start-Sleep -Milliseconds 200
            $before = if ($null -ne $script:GhosttyPickerConnection) { $script:GhosttyPickerConnection.Client } else { $null }
            $r = 'cancel' | Invoke-GhosttyPicker -CancelAfterSeconds 1 -PassThruResult
            if (-not [object]::ReferenceEquals($original, $script:GhosttyPickerConnection.Client)) { throw 'Connection changed after acceptance' }
            $results.Add(@{status=$r.status;reused=($null -ne $before -and [object]::ReferenceEquals($before, $script:GhosttyPickerConnection.Client))})
            try { 'denied' | Invoke-GhosttyPicker -Options @{preview_rows=0}; throw 'Invalid options accepted' }
            catch { if ($_ -notmatch 'capability_denied') { throw } }
            if (-not [object]::ReferenceEquals($original, $script:GhosttyPickerConnection.Client)) { throw 'Connection changed after cancellation/denial' }
            $before = if ($null -ne $script:GhosttyPickerConnection) { $script:GhosttyPickerConnection.Client } else { $null }
            $r = Invoke-GhosttyPicker -Name demo -PassThruResult
            $results.Add(@{status=$r.status;reused=($null -ne $before -and [object]::ReferenceEquals($before, $script:GhosttyPickerConnection.Client))})
            Disconnect-GhosttyPicker
            $before = if ($null -ne $script:GhosttyPickerConnection) { $script:GhosttyPickerConnection.Client } else { $null }
            $r = 'reconnect' | Invoke-GhosttyPicker -PassThruResult
            if ([object]::ReferenceEquals($original, $script:GhosttyPickerConnection.Client)) { throw 'Disconnected client was reused' }
            $results.Add(@{status=$r.status;reused=($null -ne $before -and [object]::ReferenceEquals($before, $script:GhosttyPickerConnection.Client))})
            foreach ($i in 1..2) {
            $before = if ($null -ne $script:GhosttyPickerConnection) { $script:GhosttyPickerConnection.Client } else { $null }
                $r = 'fresh' | Invoke-GhosttyPicker -FreshConnection -PassThruResult
                if ($null -ne $script:GhosttyPickerConnection) { throw 'Fresh connection was cached' }
                $results.Add(@{status=$r.status;reused=($null -ne $before -and [object]::ReferenceEquals($before, $script:GhosttyPickerConnection.Client))})
            }
            $before = if ($null -ne $script:GhosttyPickerConnection) { $script:GhosttyPickerConnection.Client } else { $null }
            $r = 'persistent-again' | Invoke-GhosttyPicker -PassThruResult
            $results.Add(@{status=$r.status;reused=($null -ne $before -and [object]::ReferenceEquals($before, $script:GhosttyPickerConnection.Client))})
            $before = if ($null -ne $script:GhosttyPickerConnection) { $script:GhosttyPickerConnection.Client } else { $null }
            $r = 'reuse-again' | Invoke-GhosttyPicker -PassThruResult
            $results.Add(@{status=$r.status;reused=($null -ne $before -and [object]::ReferenceEquals($before, $script:GhosttyPickerConnection.Client))})
            ConvertTo-Json -InputObject $results.ToArray() -Compress
        "#,
            &service.token,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut opens = 0;
        let mut saw_idle_connection = false;
        loop {
            if service.connected() && service.shared.active.lock().unwrap().is_none() {
                saw_idle_connection = true;
            }
            if let Ok(event) = service.events.recv_timeout(Duration::from_millis(20)) {
                match event {
                    Event::Open { request, session } => {
                        opens += 1;
                        assert!(service.connected());
                        session.reply(json!({"type":"opened","request_id":session.id}));
                        if matches!(request, Open::Named { .. }) {
                            session.finish("accepted", json!({"selected":[]}));
                        }
                    }
                    Event::Items {
                        sequence, session, ..
                    } => {
                        session.reply(
                            json!({"type":"ack","request_id":session.id,"sequence":sequence}),
                        );
                    }
                    Event::End { session } => {
                        session.reply(json!({"type":"input_complete","request_id":session.id}));
                        if opens != 2 {
                            session.finish("accepted", json!({"selected":[]}));
                        }
                    }
                }
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("Persistent client stalled");
            }
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let results: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(opens, 8);
        assert_eq!(results[0]["reused"], true);
        assert_eq!(results[1]["status"], "cancelled");
        assert_eq!(results[1]["reused"], true);
        assert_eq!(results[2]["reused"], true);
        assert_eq!(results[3]["reused"], false);
        for index in [4, 5] {
            assert_eq!(results[index]["status"], "accepted");
            assert_eq!(results[index]["reused"], false);
        }
        assert_eq!(results[6]["reused"], false);
        assert_eq!(results[7]["reused"], true);
        assert!(saw_idle_connection);
        let deadline = Instant::now() + Duration::from_secs(2);
        while service.connected() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!service.connected());
    }
    #[test]
    fn idle_connection_is_closed_on_disable_and_client_reconnects_when_enabled() {
        let service = Service::new(egui::Context::default()).unwrap();
        let mut policy = Policy {
            enabled: true,
            allow_stream: true,
            ..Default::default()
        };
        let pid = unsafe { GetCurrentProcessId() };
        let created = creation(unsafe { GetCurrentProcess() }).unwrap();
        service.update(&policy, pid, created);
        let mut child = run_client(
            &service,
            r#"
            $original = $script:GhosttyPickerConnection.Client
            'first' | Invoke-GhosttyPicker -PassThruResult | Out-Null
            Start-Sleep -Milliseconds 800
            'second' | Invoke-GhosttyPicker -PassThruResult | Out-Null
            [object]::ReferenceEquals($original, $script:GhosttyPickerConnection.Client) | ConvertTo-Json -Depth 16 -Compress
        "#,
            &service.token,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut opens = 0;
        let mut revoked = false;
        loop {
            if opens == 1 && !revoked && service.shared.idle.load(Ordering::Acquire) {
                policy.enabled = false;
                service.update(&policy, pid, created);
                let close_deadline = Instant::now() + Duration::from_secs(1);
                while service.connected() && Instant::now() < close_deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(!service.connected(), "Disabled idle connection stayed open");
                policy.enabled = true;
                service.update(&policy, pid, created);
                revoked = true;
            }
            if let Ok(event) = service.events.recv_timeout(Duration::from_millis(10)) {
                match event {
                    Event::Open { session, .. } => {
                        opens += 1;
                        session.reply(json!({"type":"opened","request_id":session.id}));
                    }
                    Event::Items {
                        session, sequence, ..
                    } => session
                        .reply(json!({"type":"ack","request_id":session.id,"sequence":sequence})),
                    Event::End { session } => session.finish("accepted", json!({"selected":[]})),
                }
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("Reconnect client stalled");
            }
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            false
        );
        assert_eq!(opens, 2);
        assert!(revoked);
    }
    #[test]
    fn wrong_token_never_reaches_picker_dispatch() {
        let service = Service::new(egui::Context::default()).unwrap();
        service.update(
            &Policy {
                enabled: true,
                allow_stream: true,
                ..Default::default()
            },
            unsafe { GetCurrentProcessId() },
            creation(unsafe { GetCurrentProcess() }).unwrap(),
        );
        let child = run_client(&service, "'one' | Invoke-GhosttyPicker", "wrong-token");
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(service.events.try_recv().is_err());
    }
    #[test]
    fn anonymous_effective_token_is_rejected_before_dispatch() {
        let service = Service::new(egui::Context::default()).unwrap();
        service.update(
            &Policy {
                enabled: true,
                allow_stream: true,
                ..Default::default()
            },
            unsafe { GetCurrentProcessId() },
            creation(unsafe { GetCurrentProcess() }).unwrap(),
        );
        let client = owned(unsafe {
            CreateFileW(
                wide(&format!(r"\\.\pipe\{}", service.endpoint)).as_ptr(),
                0x00100003,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
                null_mut(),
            )
        })
        .unwrap();
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        let mut header = [0; 4];
        assert!(
            transfer(
                client.0,
                &mut header,
                false,
                &service.shared,
                deadline,
                None
            )
            .is_ok()
        );
        let mut challenge = vec![0; u32::from_le_bytes(header) as usize];
        assert!(
            transfer(
                client.0,
                &mut challenge,
                false,
                &service.shared,
                deadline,
                None
            )
            .is_ok()
        );
        let challenge: Value = serde_json::from_slice(&challenge).unwrap();
        let nonce = super::super::super::wire::new_token().unwrap();
        assert!(send(client.0,&json!({"type":"authenticate","version":1,"client_nonce":nonce,"proof":hex(&proof(&service.token,&service.endpoint,challenge["nonce"].as_str().unwrap(),&nonce,"client"))}),&service.shared).is_ok());
        assert!(
            transfer(
                client.0,
                &mut header,
                false,
                &service.shared,
                deadline,
                None
            )
            .is_ok()
        );
        let mut rejection = vec![0; u32::from_le_bytes(header) as usize];
        assert!(
            transfer(
                client.0,
                &mut rejection,
                false,
                &service.shared,
                deadline,
                None
            )
            .is_ok()
        );
        let rejection: Value = serde_json::from_slice(&rejection).unwrap();
        assert_eq!(rejection["code"], "client_rejected");
        assert!(
            send(
                client.0,
                &json!({"type":"received","request_id":"authentication"}),
                &service.shared
            )
            .is_ok()
        );
        assert!(service.events.try_recv().is_err());
    }
    #[test]
    fn descendant_of_another_shell_is_rejected_with_valid_token() {
        let service = Service::new(egui::Context::default()).unwrap();
        let mut other = std::process::Command::new("pwsh")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 15",
            ])
            .spawn()
            .unwrap();
        let handle = process(other.id()).unwrap();
        service.update(
            &Policy {
                enabled: true,
                allow_stream: true,
                ..Default::default()
            },
            other.id(),
            creation(handle.0).unwrap(),
        );
        let output = run_client(&service, "'one' | Invoke-GhosttyPicker", &service.token)
            .wait_with_output()
            .unwrap();
        let _ = other.kill();
        let _ = other.wait();
        assert!(!output.status.success());
        assert!(service.events.try_recv().is_err());
    }
    #[test]
    fn powershell_named_requests_and_capability_denial() {
        for (command, permitted) in [
            (
                "Invoke-GhosttyPicker -Name demo -Parameters @{query='needle'} -PassThruResult | ConvertTo-Json -Depth 16 -Compress",
                true,
            ),
            (
                "Invoke-GhosttyPicker -Definition @{type='Command';args=@('rg','--files')}",
                false,
            ),
            ("Invoke-GhosttyPicker -Name private", false),
            (
                "'one' | Invoke-GhosttyPicker -Options @{preview=@{type='Command';args=@('pwsh.exe')}}",
                false,
            ),
        ] {
            let service = Service::new(egui::Context::default()).unwrap();
            service.update(
                &Policy {
                    enabled: true,
                    allow_stream: true,
                    named: vec!["demo".into()],
                    ..Default::default()
                },
                unsafe { GetCurrentProcessId() },
                creation(unsafe { GetCurrentProcess() }).unwrap(),
            );
            let mut child = run_client(&service, command, &service.token);
            if permitted {
                let event = service
                    .events
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                let Event::Open { request, session } = event else {
                    panic!("expected open");
                };
                match request {
                    Open::Named { name, params } => {
                        assert_eq!(name, "demo");
                        assert_eq!(params["query"], "needle");
                    }
                    _ => panic!("unexpected stream"),
                }
                session.reply(json!({"type":"opened","request_id":session.id}));
                session.finish("accepted", json!({"selected":[{"id":"0","value":"reply"}]}));
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("client stalled");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.success(),
                permitted,
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            if !permitted {
                assert!(service.events.try_recv().is_err());
            }
        }
    }
    #[test]
    fn early_result_stops_stream_and_disconnect_marks_request_cancelled() {
        for attempt in 0..8 {
            let disconnect = attempt == 7;
            let service = Service::new(egui::Context::default()).unwrap();
            service.update(
                &Policy {
                    enabled: true,
                    allow_stream: true,
                    ..Default::default()
                },
                unsafe { GetCurrentProcessId() },
                creation(unsafe { GetCurrentProcess() }).unwrap(),
            );
            let command = if disconnect {
                "'first' | Invoke-GhosttyPicker"
            } else {
                "1..100 | Invoke-GhosttyPicker -PassThruResult | ConvertTo-Json -Depth 16 -Compress"
            };
            let mut child = run_client(&service, command, &service.token);
            let Event::Open { session, .. } = service
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            else {
                panic!("expected open");
            };
            session.reply(json!({"type":"opened","request_id":session.id}));
            if disconnect {
                child.kill().unwrap();
                let _ = child.wait();
            } else {
                session.finish("accepted", json!({"selected":[{"id":"0","value":"early"}]}));
                let deadline = Instant::now() + Duration::from_secs(10);
                while child.try_wait().unwrap().is_none() {
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        panic!("early response stalled");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["status"], "accepted");
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while !session.cancelled.load(Ordering::Acquire) {
                assert!(
                    Instant::now() < deadline,
                    "disconnected request remained active"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    #[test]
    fn policy_revocation_cancels_an_idle_picker_request() {
        let service = Service::new(egui::Context::default()).unwrap();
        service.update(
            &Policy {
                enabled: true,
                allow_stream: true,
                ..Default::default()
            },
            unsafe { GetCurrentProcessId() },
            creation(unsafe { GetCurrentProcess() }).unwrap(),
        );
        let mut child = run_client(
            &service,
            "Invoke-GhosttyPicker -PassThruResult | ConvertTo-Json -Depth 16 -Compress",
            &service.token,
        );
        let Event::Open { session, .. } = service
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        else {
            panic!("expected open");
        };
        session.reply(json!({"type":"opened","request_id":session.id}));
        service.update(
            &Policy::default(),
            unsafe { GetCurrentProcessId() },
            creation(unsafe { GetCurrentProcess() }).unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("revocation stalled");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["reason"], "permission_revoked");
    }
}

enum Failure {
    Error,
}
struct ReplyPump<'a> {
    rx: &'a Receiver<Value>,
    terminal: std::cell::Cell<bool>,
    deadline: std::cell::Cell<Option<Instant>>,
}
fn pump_reply(pipe: HANDLE, state: &Shared, pump: Option<&ReplyPump<'_>>) -> IoResult<()> {
    if let Some(pump) = pump {
        if let Ok(reply) = pump.rx.try_recv() {
            send(pipe, &reply, state)?;
            if reply["type"] == "result" {
                pump.terminal.set(true);
                pump.deadline
                    .set(Some(Instant::now() + Duration::from_secs(5)));
            }
        }
    }
    Ok(())
}
type IoResult<T> = Result<T, Failure>;
impl From<String> for Failure {
    fn from(error: String) -> Self {
        let _ = error;
        Self::Error
    }
}
fn pending(
    pipe: HANDLE,
    operation: &mut OVERLAPPED,
    state: &Shared,
    deadline: Option<Instant>,
    replies: Option<&ReplyPump<'_>>,
) -> IoResult<u32> {
    loop {
        let reply_error = pump_reply(pipe, state, replies).err();
        let interrupt = if state.stop.load(Ordering::Acquire)
            || (state.idle.load(Ordering::Acquire)
                && (!state.policy.lock().unwrap().enabled
                    || state.shell.lock().unwrap().as_ref().is_none_or(
                        |shell| unsafe { WaitForSingleObject(shell.handle.0, 0) } != WAIT_TIMEOUT,
                    )))
            || deadline.is_some_and(|d| Instant::now() >= d)
            || replies.is_some_and(|pump| pump.deadline.get().is_some_and(|d| Instant::now() >= d))
        {
            Some(Failure::Error)
        } else {
            reply_error
        };
        if let Some(error) = interrupt {
            unsafe {
                CancelIoEx(pipe, operation);
                WaitForSingleObject(operation.hEvent, INFINITE);
                let mut n = 0;
                GetOverlappedResult(pipe, operation, &mut n, 0);
            }
            return Err(error);
        }
        match unsafe { WaitForSingleObject(operation.hEvent, 50) } {
            WAIT_TIMEOUT => continue,
            WAIT_OBJECT_0 => {
                let mut bytes = 0;
                if unsafe { GetOverlappedResult(pipe, operation, &mut bytes, 0) } == 0 {
                    return Err(Failure::Error);
                }
                return Ok(bytes);
            }
            _ => {
                unsafe {
                    CancelIoEx(pipe, operation);
                    let mut n = 0;
                    GetOverlappedResult(pipe, operation, &mut n, 1);
                }
                return Err(Failure::Error);
            }
        }
    }
}
fn connect(pipe: HANDLE, state: &Shared) -> IoResult<()> {
    let event = owned(unsafe { CreateEventW(null(), 1, 0, null()) })?;
    let mut operation: OVERLAPPED = unsafe { zeroed() };
    operation.hEvent = event.0;
    if unsafe { ConnectNamedPipe(pipe, &mut operation) } != 0 {
        return Ok(());
    }
    match unsafe { GetLastError() } {
        ERROR_PIPE_CONNECTED => Ok(()),
        ERROR_IO_PENDING => pending(pipe, &mut operation, state, None, None).map(|_| ()),
        _ => Err(Failure::Error),
    }
}
fn transfer(
    pipe: HANDLE,
    bytes: &mut [u8],
    write: bool,
    state: &Shared,
    deadline: Option<Instant>,
    replies: Option<&ReplyPump<'_>>,
) -> IoResult<()> {
    let event = owned(unsafe { CreateEventW(null(), 1, 0, null()) })?;
    let mut offset = 0;
    while offset < bytes.len() {
        pump_reply(pipe, state, replies)?;
        let mut operation: OVERLAPPED = unsafe { zeroed() };
        operation.hEvent = event.0;
        unsafe {
            ResetEvent(event.0);
        }
        let mut count = 0;
        let success = unsafe {
            if write {
                WriteFile(
                    pipe,
                    bytes[offset..].as_ptr(),
                    (bytes.len() - offset) as u32,
                    &mut count,
                    &mut operation,
                )
            } else {
                ReadFile(
                    pipe,
                    bytes[offset..].as_mut_ptr(),
                    (bytes.len() - offset) as u32,
                    &mut count,
                    &mut operation,
                )
            }
        };
        if success == 0 {
            if unsafe { GetLastError() } != ERROR_IO_PENDING {
                return Err(Failure::Error);
            }
            count = pending(pipe, &mut operation, state, deadline, replies)?;
        }
        if count == 0 {
            return Err(Failure::Error);
        }
        offset += count as usize;
    }
    Ok(())
}
fn send(pipe: HANDLE, value: &Value, state: &Shared) -> IoResult<()> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| Failure::Error)?;
    if bytes.len() > MAX_FRAME {
        return Err(Failure::Error);
    }
    let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
    frame.append(&mut bytes);
    transfer(
        pipe,
        &mut frame,
        true,
        state,
        Some(Instant::now() + Duration::from_secs(5)),
        None,
    )
}
fn read(
    pipe: HANDLE,
    state: &Shared,
    deadline: Option<Instant>,
    replies: Option<&ReplyPump<'_>>,
) -> IoResult<Message> {
    let mut header = [0; 4];
    transfer(pipe, &mut header[..1], false, state, deadline, replies)?;
    let partial_deadline =
        Some(deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(5)));
    transfer(
        pipe,
        &mut header[1..],
        false,
        state,
        partial_deadline,
        replies,
    )?;
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(Failure::Error);
    }
    let mut bytes = vec![0; len];
    transfer(
        pipe,
        &mut bytes,
        false,
        state,
        Some(Instant::now() + Duration::from_secs(5)),
        replies,
    )?;
    parse(&bytes).map_err(|error| {
        let _ = error;
        Failure::Error
    })
}
fn send_reply(pipe: HANDLE, reply: &Value, state: &Shared) -> IoResult<bool> {
    send(pipe, reply, state)?;
    let terminal = reply["type"] == "result" || reply["type"] == "error";
    if terminal {
        // DisconnectNamedPipe discards unread buffered bytes. Wait for explicit
        // receipt rather than blocking indefinitely in FlushFileBuffers.
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        for _ in 0..4 {
            match read(pipe, state, deadline, None)? {
                Message::Received { request_id } if reply["request_id"] == request_id => {
                    return Ok(true);
                }
                Message::Items { request_id, .. }
                | Message::EndItems { request_id, .. }
                | Message::Cancel { request_id }
                    if reply["request_id"] == request_id =>
                {
                    continue;
                }
                _ => return Err(Failure::Error),
            }
        }
        return Err(Failure::Error);
    }
    Ok(terminal)
}
fn wait_reply(pipe: HANDLE, state: &Shared, rx: &Receiver<Value>) -> IoResult<bool> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if state.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(Failure::Error);
        }
        if let Ok(reply) = rx.recv_timeout(Duration::from_millis(50)) {
            return send_reply(pipe, &reply, state);
        }
    }
}
fn serve(
    pipe: HANDLE,
    state: &Shared,
    endpoint: &str,
    token: &str,
    events: &Sender<Event>,
) -> IoResult<()> {
    let deadline = Some(Instant::now() + Duration::from_secs(5));
    let nonce = super::super::wire::new_token().map_err(|e| e.to_string())?;
    send(
        pipe,
        &json!({"type":"challenge","version":1,"nonce":nonce}),
        state,
    )?;
    let auth = read(pipe, state, deadline, None)?;

    let mut pid = 0;
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
        return Err(Failure::Error);
    }
    let client = process(pid)?;
    let Message::Authenticate {
        version: 1,
        client_nonce,
        proof: provided,
    } = auth
    else {
        return Err(Failure::Error);
    };
    if client_nonce.len() != 64
        || !client_nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || !verify_proof(token, endpoint, &nonce, &client_nonce, &provided)
    {
        return Err(Failure::Error);
    }
    if let Err(message) = authorize(pipe, client.0, state) {
        send_reply(
            pipe,
            &json!({"type":"error","request_id":"authentication","code":"client_rejected","message":message}),
            state,
        )?;
        return Ok(());
    }
    let policy = state.policy.lock().unwrap().clone();
    if !policy.enabled {
        send_reply(
            pipe,
            &json!({"type":"error","request_id":"authentication","code":"disabled","message":"Enable picker_pipe.enabled in the active Lua configuration."}),
            state,
        )?;
        return Ok(());
    }
    send(
        pipe,
        &json!({"type":"ready","version":1,"persistent":true,"proof":hex(&proof(token,endpoint,&nonce,&client_nonce,"server")),"capabilities":{"named":policy.named,"stream":policy.allow_stream},"limits":{"max_frame_bytes":MAX_FRAME,"max_item_bytes":MAX_ITEM,"max_batch_items":1024,"max_items":MAX_ITEMS,"max_storage_bytes":MAX_STORAGE,"in_flight_batches":1}}),
        state,
    )?;
    struct Connected<'a>(&'a Shared);
    impl Drop for Connected<'_> {
        fn drop(&mut self) {
            self.0.idle.store(false, Ordering::Release);
            self.0.connected.store(false, Ordering::Release);
            self.0.context.request_repaint();
        }
    }
    state.connected.store(true, Ordering::Release);
    state.context.request_repaint();
    let _connected = Connected(state);
    loop {
        state.idle.store(true, Ordering::Release);
        let request = read(pipe, state, None, None)?;
        state.idle.store(false, Ordering::Release);
        let request_id = match &request {
            Message::Open { request_id, .. } => request_id.clone(),
            _ => return Err(Failure::Error),
        };
        serve_request(pipe, state, client.0, events, request)?;
        // The request guard is dropped before confirming that the next open
        // may safely reuse this connection. Result receipt drains in-flight input.
        send(pipe, &json!({"type":"idle","request_id":request_id}), state)?;
        match read(
            pipe,
            state,
            Some(Instant::now() + Duration::from_secs(5)),
            None,
        )? {
            Message::Received {
                request_id: received,
            } if received == request_id => {}
            _ => return Err(Failure::Error),
        }
    }
}

fn serve_request(
    pipe: HANDLE,
    state: &Shared,
    client: HANDLE,
    events: &Sender<Event>,
    request: Message,
) -> IoResult<()> {
    let Message::Open {
        request_id,
        request,
    } = request
    else {
        return Err(Failure::Error);
    };
    if request_id.is_empty() || request_id.len() > 128 {
        return Err(Failure::Error);
    }
    authorize(pipe, client, state)?;
    let permission = state.policy.lock().unwrap().authorize(&request);
    if let Err(message) = permission {
        send_reply(
            pipe,
            &json!({"type":"result","request_id":request_id,"status":"error","code":"capability_denied","message":message}),
            state,
        )?;
        return Ok(());
    }
    let stream = matches!(request, Open::Stream { .. });
    let capability = match &request {
        Open::Named { name, .. } => Capability::Named(name.clone()),
        Open::Stream { .. } => Capability::Stream,
    };
    let (session, rx) = Session::new(request_id);
    *state.active.lock().unwrap() = Some((session.clone(), capability));
    struct Disconnect<'a> {
        session: Arc<Session>,
        context: &'a egui::Context,
        active: &'a Mutex<Option<(Arc<Session>, Capability)>>,
    }
    impl Drop for Disconnect<'_> {
        fn drop(&mut self) {
            self.session.cancelled.store(true, Ordering::Release);
            self.active.lock().unwrap().take();
            self.context.request_repaint();
        }
    }
    let _disconnect = Disconnect {
        session: session.clone(),
        context: &state.context,
        active: &state.active,
    };
    events
        .try_send(Event::Open {
            request,
            session: session.clone(),
        })
        .map_err(|_| Failure::Error)?;
    state.wake_ui();
    if wait_reply(pipe, state, &rx)? {
        return Ok(());
    }
    let mut sequence = 0;
    let mut ended = false;
    let mut total = 0;
    let mut count = 0;
    let mut ids = HashSet::new();
    let pump = ReplyPump {
        rx: &rx,
        terminal: std::cell::Cell::new(false),
        deadline: std::cell::Cell::new(None),
    };
    let mut drained = 0;
    loop {
        let message = read(pipe, state, pump.deadline.get(), Some(&pump))?;
        pump_reply(pipe, state, Some(&pump))?;
        if pump.terminal.get() {
            match message {
                Message::Received { request_id } if request_id == session.id => return Ok(()),
                Message::Items { request_id, .. }
                | Message::EndItems { request_id, .. }
                | Message::Cancel { request_id }
                    if request_id == session.id && drained < 4 =>
                {
                    drained += 1;
                    continue;
                }
                _ => return Err(Failure::Error),
            }
        }
        if session.done() {
            if wait_reply(pipe, state, &rx)? {
                return Ok(());
            }
            continue;
        }
        authorize(pipe, client, state)?;
        // A config reload can revoke all access or individual capabilities.
        if !state.policy.lock().unwrap().enabled {
            return Err(Failure::Error);
        }
        let event = match message {
            Message::Cancel { request_id } if request_id == session.id => {
                session.cancelled.store(true, Ordering::Release);
                session.finish("cancelled", json!({"reason":"client"}));
                state.context.request_repaint();
                if wait_reply(pipe, state, &rx)? {
                    return Ok(());
                }
                continue;
            }
            Message::Items {
                request_id,
                sequence: received,
                items,
            } if stream && !ended && request_id == session.id && received == sequence => {
                if items.is_empty() || items.len() > 1024 {
                    return Err(Failure::Error);
                }
                for item in &items {
                    let bytes = item.text.len()
                        + serde_json::to_vec(&item.value)
                            .map_err(|_| Failure::Error)?
                            .len()
                        + item.id.len();
                    if item.id.is_empty()
                        || item.id.len() > 128
                        || bytes > MAX_ITEM
                        || !ids.insert(item.id.clone())
                    {
                        return Err(Failure::Error);
                    }
                    total += bytes;
                    count += 1;
                    if total > MAX_STORAGE || count > MAX_ITEMS {
                        return Err(Failure::Error);
                    }
                }
                sequence += 1;
                Event::Items {
                    sequence: received,
                    items,
                    session: session.clone(),
                }
            }
            Message::EndItems {
                request_id,
                last_sequence,
            } if stream
                && !ended
                && request_id == session.id
                && last_sequence == sequence.checked_sub(1) =>
            {
                ended = true;
                Event::End {
                    session: session.clone(),
                }
            }
            _ => return Err(Failure::Error),
        };
        if session.done() {
            if wait_reply(pipe, state, &rx)? {
                return Ok(());
            }
            continue;
        }
        events.try_send(event).map_err(|_| Failure::Error)?;
        state.wake_ui();
        if wait_reply(pipe, state, &rx)? {
            return Ok(());
        }
    }
}
