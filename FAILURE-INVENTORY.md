# Failure feedback inventory

Initial source review of the egui application, its C shim, and selected embedded
Ghostty entry points. This is an implementation planning inventory, not a claim
that every error in the shared Zig core or external dependencies has been audited.
No runtime error behavior is changed by this document.

## Classification

Classify by cause and consequence independently of the reporting mechanism.
An `expect` can conceal an ordinary operational failure; a `false` return can
mean either failure or a normal absence of results.

| Class | Meaning | Default feedback |
| --- | --- | --- |
| Configuration | User settings cannot be read, evaluated, or validated | Persistent active issue; Messages; explain fallback and restart |
| Operational | A requested operation cannot complete | Messages plus a brief notification; allow retry where appropriate |
| Degraded | Operation continues with reduced capability | Warning in Messages; explain fallback |
| Fatal startup | App cannot create its usable UI | Native dialog with copyable details; then exit |
| Internal | A programming invariant or worker fails unexpectedly | Diagnostic report; preserve debugging context; controlled shutdown if necessary |
| Expected control flow | Cancellation, no match, unavailable action, stale work, shutdown | Usually no warning; contextual empty state where useful |

Keep severity (info/warning/error), lifetime (active condition/history), and
recovery (retry/restart/unavailable) as separate attributes. Closing or reading
Messages must not clear an active condition. A historical action failure should
not imply that the app remains broken forever.

## Reviewed failure families

Addressed paths:

- **C01, C02:** configuration fallback with persistent status and Messages.
- **S01:** event-loop creation and execution use the shared fatal-error path.
- **S02:** graphics adapter and device selection, the window, and its surface
  use the shared fatal-error path.
- **S03:** terminal render operations that fail report to the error history and
  stderr; Ghostty marks that renderer unhealthy and the app keeps running.
- **S05:** main-terminal initialization preserves the native failed stage and
  error name, cleans partial resources, and uses the shared fatal-error path.
  Recoverable configuration diagnostics still use the existing fallback.
- **S04:** secure token generation errors stop main startup through the shared
  fatal-error path. Floating creation is canceled with a toast instead. No
  predictable token fallback is used.
- **R01:** a lost or outdated window surface is reconfigured once; a failure
  that persists stops further rendering, exits the event loop, and reports the
  failed step and underlying error through the native dialog.
- **C03, I04:** invalid startup arguments fail before terminal startup, with
  a native dialog and exit status 1.
- **O07, O08, O10:** command, preview, and search workers report active-operation
  failures with local feedback and history. Search can recreate its worker on a
  later request; subprocess diagnostics include bounded stderr and exit status.
- **O03, O05, O12, S06:** app-owned snapshot failures cancel the affected operation;
  caught synchronous native errors preserve their causes across the C shim.
- **I01–I03, E01:** UI and C callback panic boundaries use fatal reporting;
  optional worker failures remain recoverable. Expected shutdown stays quiet.

The shared fatal path logs to stderr, shows a Windows native dialog, and exits
with status 1. Ordinary fatal errors are shown after the event loop returns or
fails to start. A panic inside a Rust C callback reports and exits immediately
to prevent unwinding across C. Other platforms currently use stderr. No automatic
graphics retry or context recovery is implemented. The table identifies partial
coverage and the deeper shared-core and external-worker audit limits.

Paths below are relative to this repository. Paths starting with `../../`
refer to the `ghostty` submodule; read `../../` as `ghostty/`.
Related propagation sites are grouped rather than counted as separate failures.

| ID | Source / operation | Current behavior | Class / recovery | Status and follow-up |
| --- | --- | --- | --- | --- |
| C01 | `src/config.rs`: read_config, AppConfig::parse, ghostty_args, FloatingPadding::validate; `src/picker/config.rs`: Bindings compilation | Lua errors reject the attempted configuration; defaults and persistent Config indicator; original error in Messages | Configuration / fix and restart | Implemented; improve structured file/setting/location metadata later |
| C02 | `src/ghostty_shim.c`: config load/finalize | Native diagnostics copied into Messages before config is freed | Configuration / fix and restart | Implemented; preserve original diagnostic and accurately describe partial application |
| C03 | `src/config.rs`: prepare argument parsing; `src/main.rs` | Invalid invocation returns FatalError before graphics/terminal startup | Invalid invocation / correct arguments | Implemented: native startup dialog, stderr, exit status 1 |
| S01 | `src/app.rs`: run | Event-loop creation/run errors return to main | Fatal startup or runtime / environment-dependent | Implemented: Windows native error dialog, stderr fallback, exit status 1 |
| S02 | `src/gpu.rs`: SharedGpu::new; `src/app.rs`: WindowSurface::new | No adapter, an adapter without immediate data, device creation, window/surface creation, and an unsupported surface format return a shared FatalError | Fatal startup / graphics environment | Implemented: exit event loop, Windows native dialog identifying failed step and underlying error, exit status 1 |
| S03 | `src/gpu/bridge.rs`: dispatch; `src/gpu.rs`: uncaptured device errors | A panicking render operation returns failure to Ghostty, which marks the renderer unhealthy; wgpu validation errors, including custom shader compile failures, are logged | Degraded runtime / driver failure or internal bug | Implemented: error history and stderr; the app and other panes keep running |
| S04 | `src/picker/protocol.rs`: new_token | Randomness acquisition returns FatalError, preserving the RNG error; no fallback token | Fatal main startup; floating operation failure / environment | Implemented: native startup dialog and exit status 1 for main; floating creation canceled with a toast, existing terminal keeps running |
| S05 | `src/terminal/pane.rs`: TerminalPane::create; `src/ghostty_shim.c`: ghostty_egui_host_create | Fatal init/config/app/surface/allocation failures return stage and native cause; Rust returns FatalError | Fatal main startup / restart or correct underlying cause | Implemented: shared native dialog, stderr, exit status 1; partial resources freed; recoverable config diagnostics retain fallback |
| S06 | `../../src/apprt/embedded.zig`: ghostty_app_new, ghostty_surface_new; `../../src/termio/Exec.zig`: command setup/start | Caught synchronous embedded errors preserve native error names; app/surface creation preserves causes and runtime operations report to history/toasts | Operational / command, cwd, PTY, resources | Implemented for caught synchronous operations; deep asynchronous Zig renderer/PTY logs and external worker failures still require auditing |
| O01 | `src/terminal.rs`: execute_action NewWindow; `src/app.rs`: open_new_window | Process launch failure uses a toast, bounded history, and stderr | Operational / retry | Implemented: error history + notification with the OS error |
| O02 | `src/terminal/pane.rs`: create_floating; `src/terminal.rs`: toggle action; shim floating creation | Native floating creation preserves failed stage/cause; renderer/token failures also return errors; all show a toast, enter history, and retain stderr | Operational / retry | Implemented: bounded error history + notification; existing main terminal stays usable |
| O03 | `src/terminal/pane.rs`: open picker context read and factory resolve | Cursor/factory/snapshot acquisition failures use local notification and bounded history/stderr | Operational or configuration callback / retry or fix and restart | Implemented for app-owned failures; unavailable snapshots cancel opening instead of silently continuing |
| O04 | `src/terminal/pane.rs`: Lua action callback | Lua action failure shows a toast and enters bounded history and stderr | Operational or script error / cause-dependent | Implemented: full error retained within the detail limit |
| O05 | `src/terminal/pane.rs`: picker accept callback and snapshot navigation | Callback and snapshot/navigation failures retain diagnostics and cancel the affected operation | Operational / rerun picker | Implemented for app-owned paths; normal no-match and boundary cases remain quiet |
| O06 | `src/picker/source.rs`, `src/picker/action.rs`: items provider and filesystem root resolution | Provider/root/source-worker failures retain local error, history, stderr; source worker spawn/panic failures do not take down the main UI | Operational / retry or correct settings | Implemented for app-owned sources; external filesystem/search-library internal workers remain outside this audit |
| O07 | `src/picker/command.rs`: command source spawn/read/wait/nonzero exit | Spawn/read/wait/reader-join/worker failures retain command, cwd, status, bounded stderr; existing partial results remain available | Operational / retry | Implemented: local error + history/stderr; ripgrep exit 1 is no-match, other commands default to exit 0; success_exit_codes can override |
| O08 | `src/picker/preview.rs`, `src/picker/preview_vt.c`: preview execution/read/render | Preview process exit/stdout/stderr/read/join/worker failures remain inline and enter history/stderr | Operational / retry or select another item | Implemented: bounded stdout/stderr capture and process cleanup; canceled/stale results stay quiet |
| O09 | `src/search.rs`: regex evaluation | Regex error returned in SearchOutcome for search UI | User input / edit query | Keep inline validation; avoid accumulating an error for each keystroke |
| O10 | `src/search.rs`: worker creation, request channels, cursor operations | Worker starts lazily; creation/send/disconnection/panic failures stop the request without stopping the app; snapshot scan failure returns an error | Operational/internal / restart worker or operation | Implemented: notification/history/stderr and recreation on a later user request; regex errors remain inline |
| O11 | `src/terminal/pane.rs`: enter_copy_mode | Snapshot creation/cursor initialization errors use a toast, history, and stderr | Operational / retry | Implemented for these explicit failures |
| O12 | `src/terminal/pane.rs`: quick select, paging, copy movement/decorations, prompt navigation; shim snapshot APIs | Invalid/expired snapshot failures cancel affected overlays/navigation and preserve diagnostics; native caught errors cross the shim | Mixed operational and expected boundary/no-match cases | Implemented for app-owned operations; normal boundary/no-match results remain quiet, and unexposed shared-core failures remain an audit limit |
| O13 | `src/terminal/pane.rs`: protocol callback and OSC request handling | Rejected protocol and unknown-picker requests enter history/stderr; authentication token not logged; busy-overlay requests remain stderr only | Invalid external input or busy state / caller-dependent | Partially addressed: repeat aggregation implemented; normal overlay contention is excluded; oversized/null payloads are still dropped |
| D01 | `src/picker/font.rs`: terminal font export | Terminal font export failure retains monospace fallback and records warning in history/stderr | Degraded / continues | Implemented: identical fallback warnings aggregate across regular/bold fonts |
| D02 | `src/app.rs`: WindowSurface::new present mode | Mailbox presentation falls back to AutoNoVsync when the surface lacks it | Degraded / continues | Implemented: no failure path |
| R01 | `src/app.rs`: WindowSurface::acquire | Timeout or occlusion skips the frame; a lost or outdated surface is reconfigured once; other failures record a FatalError, stop rendering, and exit the event loop | Fatal runtime graphics failure / restart | Implemented: shared Windows native dialog with failed step and underlying error, stderr, exit status 1 |
| I01 | `src/app.rs`, `src/terminal.rs`, `src/terminal/pane.rs`, `src/painter.rs`, `src/picker.rs` | Assertions remain; unexpected UI panics use controlled native fatal reporting | Internal invariant / bug | Implemented boundary handling; diagnostic message retained, unknown UI state is not reused |
| I02 | Shared mutexes in pane/picker/source/command code | All sample Rust C callbacks catch panics before returning across C; optional workers catch and report failure; diagnostic-only mutexes recover their bounded state | Internal / bug | Implemented: UI/shared-state poisoning is fatal rather than silently reused; aborts/access violations cannot be caught as Rust panics |
| I03 | `src/actions.rs`, `src/input.rs`, `src/quick_select.rs`, `src/copy_mode.rs`, `src/picker/source.rs` | Justified internal assertions remain; UI/worker/C callback boundaries handle unwinding panics | Internal / bug | Implemented boundary handling; assertions are not indiscriminately converted into fallback results |
| I04 | `src/main.rs`: CString argument conversion | Argument Unicode/NUL validation returns FatalError before terminal startup | Validation/internal boundary | Implemented: native dialog/stderr/exit status 1; invalid argument contents are not echoed |
| E01 | Wake/frame proxy send; worker update channels; cancellation/cleanup kill/wait/join | Optional worker creation/panic/channel failures are reported; normal cancellation/closed receivers and best-effort shutdown cleanup remain quiet | Usually shutdown/cancellation; possibly worker failure | Implemented for app-owned worker boundaries; shutdown errors are excluded unless they indicate an actual active-operation failure |

## Recoverable error history

`src/error_log.rs` provides a thread-safe session history shared by main/floating
panes and workers. It retains at most 200 records, caps details at 16 KiB per
record (UTF-8 safe), and bounds summary/source text. Matching failures aggregate
with a repeat count and move to the newest position. Records include UTC time,
severity, source, summary, and details. Reporting also writes to stderr.

Use **Error history** in the command palette, `OpenErrorLog`, or a named Lua
picker of `type = "ErrorLog"`. Search to preview a row's full details; Enter closes the
picker. A picker holds a snapshot, so eviction does not
invalidate a selected record; reopen it to see newer errors. Show Messages also
includes the history. Persistent configuration issues are held separately and
are never evicted by runtime failures. History is not written to disk and resets
on restart. Expected cancellation/no-match/busy-overlay events do not enter it.

## Shared core boundary follow-up

Caught synchronous errors in the embedded API now expose a thread-local native
cause. The shim forwards causes for the runtime operations it uses, including
frame retrieval, resize/scale, input, app ticks, and snapshots. These enter the
bounded history and notifications; identical records aggregate. Deep asynchronous
renderer and PTY failures still need a separate reporting bridge. Follow the APIs
actually used by `src/ghostty_ffi.rs` and `src/ghostty_shim.c`; do not surface every
shared-core log line indiscriminately.

Working-directory checks in embedded surface initialization can warn and fall
back; shell integration/resource discovery in Exec can degrade without failing
startup. These need accurate fallback descriptions rather than blanket fatal
errors. Clipboard unsupported, absent selection, empty results, normal child
exit, and unsupported actions are not automatically errors.

## Recommended implementation order

1. Extend the implemented structured history with optional location metadata
   where a source actually provides it. Configuration persistence stays separate.
2. Audit deep asynchronous Zig renderer/PTY failures and external-library workers;
   app-owned worker/read/join and invalid snapshot paths now report failures.
3. Audit remaining C API uses and silent bool/Option/ignored-result sites
   individually, preserving normal absence, boundaries, and cancellation.
4. Describe optional shell integration/resource and working-directory fallbacks
   accurately when adding their warnings.
5. Extend existing worker and subprocess failure tests with native stale-snapshot
   and graphics failure injection.

## Review limits

This pass groups production failure families from Rust source, including Result
propagation, stderr, toasts, inline errors, expects/asserts, and silent returns.
Tests and normal Option/false control flow are excluded from user-facing failure
counts. App-owned snapshot failure handling and caught synchronous native causes
are covered. Deep Zig renderer/PTY implementations, external filesystem/search
crates, OS/driver behavior, and unexposed native failures need further tracing.
Do not use the number of grep matches as a count of distinct failure scenarios.
