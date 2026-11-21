# Pasting a picture into a shell on another machine

Status: built 2026-11-18, as below, and run for real (see "What phase 0
found"). The two open questions were built to their recommendations. Kept as
the record of the design; `docs/configuration.md` is the user's account.

## What phase 0 found (2026-11-18)

In a `--remote` window over `ssh vm-self`, with Claude Code 2.1.328 in the
pane, the bare path pasted (a bracketed paste, nothing after it) became
`[Image #1]` in Claude's prompt, and Claude read the picture: asked the colour
of a marker drawn on it, it said magenta. So `clipboard.claude_path` gives the
path as it is. A path with a space in it was not tried: the folder is under
the user's profile, which had none here.

## Goal

With a picture on the clipboard of the machine the window is on, one action
puts that picture in a file on the machine the pane's shell is on, and types
the file's path at the shell. Claude Code running in that shell then picks the
picture up from the path.

It works the same in a `--remote` window and a local one, because the file
travels over the connection the window already has to the session daemon. No
second `ssh`, no `scp`, no address written into a config file.

nterm itself gets two general pieces, each usable on its own from Lua: read
what is on the clipboard (text, a picture, copied files), and send bytes to
the pane's machine as a file. The
paste-a-picture action is a few lines of Lua shipped with nterm that puts the
two together. A user can put them together differently.

## The Lua API

Both are methods of the `app` a window's Lua function is given: in `actions`,
in a picker's `action` and `keys`, and in a quick select's `action`. They are
not in `nterm-daemon.lua`, whose functions run on the daemon's machine, which
has no clipboard of the user's.

```lua
-- What is on this machine's clipboard, of the kinds asked for.
app:clipboard({ "text", "image", "files" }, function(app, clip, why) ... end)

-- `bytes` kept as a file on the machine the pane's shell is on.
app:send_file(bytes, { extension = "png" }, function(app, path, why) ... end)
```

`clip` has a field for each kind asked for that the clipboard holds, and
none for the others:

- `clip.text`: the text, as UTF-8.
- `clip.image`: the picture, as the bytes of a PNG file.
- `clip.files`: the files copied in Explorer (or another file manager), as a
  list of their full paths on this machine.

A clipboard holds the same thing in several forms at once (a picture copied
in a browser is a picture and HTML; a file copied in Explorer is a file list
and may be its name as text), so several fields can be there together. Only
the kinds asked for are read: encoding a picture nobody wanted is the slow
part. Why there are kinds and not raw bytes: what Windows keeps on the
clipboard are formats of its own (a picture is a Windows bitmap with a
header, a file list a Windows structure), useless as they are to anything on
the other end. Each kind is one form a program elsewhere can use. More kinds
can be added later (`html` is one arboard can read already) without changing
what these are.

- **Both answer later, through the function given.** Reading the clipboard
  and encoding a large screenshot takes a noticeable part of a second, and
  sending a file over `ssh` takes longer; neither may freeze the window. So
  each method only asks, and returns at once; nterm calls the function when
  the answer is there, on a later frame.
- **The function is given a new `app`,** for the same pane, which can do all
  that an action's `app` can: `paste`, `send_text`, `replace_prompt`,
  `show_toast`, and the two methods here again. (The `app` an action is
  given stops working when the action returns; that rule stays.)
- **Success:** `clip` (a table, empty when the clipboard holds none of the
  kinds asked for, which is not a failure) or `path` (the file's full path on
  the shell's machine, as that machine writes paths). Bytes are Lua strings,
  which hold any bytes.
- **Failure:** `nil` and `why`, a sentence for a person: the clipboard could
  not be opened (another program holds it); an unknown kind was asked for;
  the pane is not run by a session daemon; the daemon there is
  older than this window; the file is over 32 MiB; the daemon did not answer
  within 30 seconds; the connection was lost. nterm shows nothing itself; the
  function decides.
- **If the pane is gone** by the time the answer comes, the function is not
  called. A file already written stays and is cleaned up later.
- `extension`: 1 to 10 ASCII letters and digits; optional, `bin` without one.
  `bytes` may be any bytes: a picture, or a file read with `io.open`.
- A function raising an error is shown as an action's error is
  ("Action failed").

The action shipped with nterm, in `lua/nterm/clipboard.lua` (found by
`require` as `lua/nterm/git.lua` is):

```lua
local M = {}

-- What Claude Code takes as an image. Phase 0 settles it; a bare path
-- until then.
function M.claude_path(path)
  return path
end

-- An action: the clipboard's picture, sent to the shell's machine, and its
-- path pasted at the prompt. `options.format(path)` gives what is pasted.
function M.paste_image(options)
  local format = (options or {}).format or M.claude_path
  return function(app)
    app:clipboard({ "image" }, function(app, clip, why)
      if not clip then return app:show_toast("Paste picture", why, 3000) end
      if not clip.image then
        return app:show_toast("Paste picture", "The clipboard holds no picture.", 3000)
      end
      app:send_file(clip.image, { extension = "png" }, function(app, path, why)
        if not path then return app:show_toast("Could not paste picture", why, 5000) end
        app:paste(format(path))
      end)
    end)
  end
end

return M
```

In `nterm.lua`:

```lua
local clipboard = require("nterm.clipboard")
return {
  actions = {
    PasteImage = { label = "Paste picture from clipboard", action = clipboard.paste_image() },
  },
}
```

Other things the same two pieces make, with no change to nterm:

```lua
-- Quoted for PowerShell, for a shell and not for Claude.
clipboard.paste_image { format = require("nterm.git").quote.powershell }

-- A file of this machine, chosen in a picker run here, sent to the shell's
-- machine and its path typed there.
SendFile = {
  type = "FileSystem", roots = { os.getenv("USERPROFILE") }, files_only = true,
  action = function(app, item)
    local file = assert(io.open(item.path, "rb"))
    local bytes = file:read("a")
    file:close()
    app:send_file(bytes, { extension = item.path:match("%.(%w+)$") }, function(app, path, why)
      if path then app:paste(path) else app:show_toast("Could not send", why, 5000) end
    end)
  end,
},

-- Files copied in Explorer, each sent to the shell's machine, their paths
-- typed there with a space between.
PasteFiles = { label = "Paste copied files", action = function(app)
  app:clipboard({ "files" }, function(app, clip, why)
    local files = clip and clip.files
    if not files then return app:show_toast("Paste files", why or "No files were copied.", 3000) end
    local paths, left = {}, #files
    for i, local_path in ipairs(files) do
      local file = assert(io.open(local_path, "rb"))
      local bytes = file:read("a")
      file:close()
      app:send_file(bytes, { extension = local_path:match("%.(%w+)$") }, function(app, path, why)
        paths[i] = path or ("<" .. why .. ">")
        left = left - 1
        if left == 0 then app:paste(table.concat(paths, " ")) end
      end)
    end
  end)
end },

-- Whatever is there: a picture is sent, text is pasted as it is.
PasteAnything = { label = "Paste picture or text", action = function(app)
  app:clipboard({ "image", "text" }, function(app, clip, why)
    if not clip then return app:show_toast("Paste", why, 3000) end
    if clip.image then
      app:send_file(clip.image, { extension = "png" }, function(app, path, why)
        if path then app:paste(path) else app:show_toast("Could not paste picture", why, 5000) end
      end)
    elseif clip.text then
      app:paste(clip.text)
    end
  end)
end },

-- The clipboard's picture saved on this machine, with no daemon at all.
SaveScreenshot = { label = "Save clipboard picture", action = function(app)
  app:clipboard({ "image" }, function(app, clip, why)
    if not (clip and clip.image) then
      return app:show_toast("Save picture", why or "The clipboard holds no picture.", 3000)
    end
    local name = os.getenv("USERPROFILE") .. "\\Pictures\\clip-" .. os.time() .. ".png"
    local file = assert(io.open(name, "wb"))
    file:write(clip.image)
    file:close()
    app:show_toast("Saved", name, 3000)
  end)
end },
```

(`PasteFiles` reads each file on the window's thread with `io.open`, so a
large one freezes the window while it is read. Fine for a few files of a few
MB; a `read_file` that answers later is the fix if it is ever needed.)

## How it works

1. The action calls `app:clipboard({ "image" }, ..)`. A worker thread in the
   window reads the clipboard's picture as a PNG; the function given gets it.
2. That function calls `app:send_file`. The window sends the bytes to the
   pane's daemon in one new frame.
3. The daemon writes them to a folder of its own under a name of its own
   choosing, and answers with the full path.
4. The function given to `send_file` pastes the path at the shell.

The window never names a path on the daemon's machine, and the daemon never
runs or opens what it writes. A window on another machine still only shows
and types. Writing any bytes, not only pictures, gives a window nothing new:
it can already type anything at the shell.

## Decisions already made, and why

- **Later functions, not return values.** What an action asks of `app` is
  collected and done after the function returns (`Command` in
  `src/lua_callbacks.rs`, applied in `Pane::finish_lua_action`). A method
  that returned the path would have to wait on the network inside the
  function, which freezes the window. One mechanism, "call this function
  later with a new `app` for the same pane", serves both methods, and is
  there for anything slow added later.
- **The clipboard is read on a worker too,** for the same reason, which is
  why `clipboard` takes a function as well and is not a plain
  `nterm.clipboard()`.
- **A frame of bytes, not JSON.** Files are megabytes. The protocol already
  carries bytes unencoded in their own frame kinds (`Frame::Snapshot`,
  `Frame::Previewed`).
- **One frame per file, with a size limit.** `MAX_FRAME` is 1 GiB, far over
  what is needed; the limit is 32 MiB. The window's writer thread sends frames
  in order, so on a slow link typing waits behind a large file until it is
  sent. Accepted for now; say so in the docs.
- **The clipboard picture is always PNG.** The clipboard gives raw pixels, and
  PNG keeps a screenshot exact.
- **Daemon-off mode is not supported.** It is going away. `send_file` in a
  pane with no session link answers `nil, why`.

## Decided with the owner (2026-11-18)

1. **No key for now.** It is an action in the command palette
   (Ctrl+Shift+P), and can be bound in `keys` like any action. Ctrl+V and
   default keys are left for later; see "Not in this piece".
2. **No protocol bump.** `PROTOCOL_VERSION` is bumped when something that
   exists changes meaning (16 was, because `PickerOpen.pane` became
   optional). This only adds: a frame kind and a message nothing older sends
   or expects. A daemon older than this decodes the new frame as
   `Control::Unknown` and ignores it, and `nterm-daemon --relay` copies bytes
   without reading them. A bump would also make a new window refuse every
   daemon already running, and those hold live shells.
   So that a window need not wait to learn a daemon is older, the daemon says
   it can keep files in its `Welcome`: a new field `files: bool` with
   `#[serde(default)]`. An older daemon leaves it out, which reads as false,
   and an older window ignores it (unknown fields are tolerated; see
   `messages_from_a_newer_version_are_tolerated`). `send_file` then answers
   at once "the session daemon there is older than this window" instead of
   sending anything. The 30-second wait stays, for a daemon that says yes and
   then does not answer.
3. **Where and how long:** `%LOCALAPPDATA%\nterm\pasted\` on the daemon's
   machine (beside its logs, see `log::directory()`); files older than 7 days
   are removed whenever a new one is written.
4. **What is typed: whatever Claude Code takes as an image.** Phase 0 finds
   out exactly; `clipboard.claude_path` is written to what it finds.

## Open questions for the owner

- **Any file, or pictures only?** This plan lets `send_file` send any bytes
  with a safe extension (see "How it works" for why that gives a window
  nothing new). Recommended: any file.
- **In the palette with no config?** This plan puts the action in
  `nterm.example.lua`, so it is there once `nterm.lua` has the lines above.
  Recommended: that, for now.

## Phase 0: find what Claude Code expects (by hand, 10 minutes)

What is known: Claude Code takes an image three ways: pasted from its own
machine's clipboard (useless over `ssh`, where that clipboard is the VM's),
dragged onto the terminal (which types the file's path), or a path to an
image in the prompt. So a path at the prompt works one way or another. What
has to be found:

- whether a pasted path turns into an attachment in the prompt (shown as
  `[Image #1]`), or stays text that Claude reads with its file tool
  afterwards. Both work; the first is what to aim for;
- whether that depends on the paste being a bracketed paste (nterm's
  `nterm_host_text`, which `app:paste` uses, is one) or on what follows the
  path (a space, nothing);
- what a path with a space in it needs: quotes, or nothing. The folder is
  under the user's profile, and a user name can have a space.

How: in an nterm pane on the VM, over `ssh` (`nterm --remote vm-self`), start
`claude`, put a PNG's full path on the clipboard as text, and paste it. Then
the same with a trailing space, in quotes, and in a folder with a space. Write
what was seen into this file. `clipboard.claude_path` is whatever made
`[Image #1]` appear, quoting a path only when it has a space if that is what
was needed. Do not build past phase 1 until this is known.

## Phase 1: protocol and daemon

`crates/nterm-session/src/protocol.rs`

- `Frame::File { request: u32, extension: String, bytes: Vec<u8> }`, UI to
  daemon, new kind byte `KIND_FILE = 8`. Body: the request in the id slot,
  then one byte giving the extension's length, the extension, then the bytes.
  An extension that is not 1 to 10 ASCII letters and digits fails to decode
  as a bad frame would.
- `Control::FileKept { request: u32, path: String }`, daemon to UI.
- `files: bool` on `Control::Welcome`, `#[serde(default)]` (decision 2).
- Failure is the existing `Control::Error { request: Some(..), message }`.
- Add both to `frames_round_trip`, a test that a bad extension is refused,
  and one that a `Welcome` without `files` decodes with it false.
- Do not change `PROTOCOL_VERSION`.

`crates/nterm-daemon/src/files.rs` (new, declared in `lib.rs`)

- `keep(directory, extension, bytes) -> Result<PathBuf, String>`.
- Refuse anything over 32 MiB, and check the extension again (lowercase it).
  Nothing about what the bytes are is checked.
- Name: `pasted-<unix seconds>-<daemon pid>-<counter>.<extension>`, opened
  with `create_new` so nothing is ever overwritten. The pid is there because
  several daemons (one per session) share the folder, and each counts from
  one. Should the name exist anyway, count on and try again a few times.
- Before writing, clean the folder:
  - look only at what is directly in it, not in folders under it;
  - only plain files (not folders, not links) whose names have exactly the
    shape above: `pasted-`, three runs of digits joined by `-`, a `.`, and an
    extension;
  - remove those last changed more than 7 days ago;
  - touch nothing else there;
  - a file that is already gone (another daemon cleaned it first) is not an
    error, and a failure to clean is not a failure to keep: log it and carry
    on.
- Log one line per file kept (size and path), as the rest of the daemon does.

`crates/nterm-daemon/src/server.rs`, in `converse`

- Send `files: true` in the `Welcome`.
- In the `match frame` at the top of the loop (about line 602), take
  `Frame::File` from a UI: call `files::keep`, answer `FileKept` or `fail`.
  Add `Frame::File` to nothing else; `relay` and `pick` already ignore frames
  they do not know.

Tests

- Unit tests of `keep` against a temp folder: a file is written and reads
  back the same; too large and a bad extension are refused; old files of the
  right shape go, and other files, new ones of the right shape, and anything
  in a folder under it stay; a name already taken is not overwritten.
- A session test in `crates/nterm-daemon/tests/session.rs`: send a
  `Frame::File` of about 3 MB, expect `FileKept`, check the file's bytes and
  extension, delete it; send one over the limit, expect `Error` with the same
  request. `Ui` only sends `Control` today, so it needs a small way to send
  any frame. Run it with `NTERM_TEST_RELAY=1` as well: that is the proof a
  large frame passes through `nterm-daemon --relay`.

## Phase 2: the window

`Cargo.toml`

- `arboard` with `default-features = false, features = ["image-data"]`, and
  `png`. Both are already in `Cargo.lock` (arboard 3.6.1 through `egui-winit`,
  png 0.18.1 through `image`); ask for versions that resolve to those, so
  nothing new is downloaded or built.

`src/clipboard.rs` (new)

- `read(kinds) -> Result<Clip, String>`, where `Clip` has `text`, `image`
  (PNG bytes) and `files`, each an `Option`, filled only for the kinds
  asked. A kind the clipboard does not hold is `None`, not an error; failing
  to open the clipboard is an error. Only ever called on a worker thread.
- Text: `arboard` `get_text`. Files: `arboard` `get().file_list()`.
- Picture: when the clipboard holds Windows's registered `"PNG"` format
  (browsers and many programs put one there), take those bytes as they are,
  through `clipboard-win` (already a dependency, through `arboard`). arboard's
  own `get_image` decodes that PNG to pixels, which would then have to be
  encoded again. Otherwise `get_image` (it reads `CF_DIBV5`) and encode the
  pixels with `png`.
- Watch for this: some Windows programs put pictures on the clipboard with
  every pixel's alpha at zero, which would give an invisible PNG. If every
  alpha byte is zero, set them all to 255. Check with a real Snipping Tool
  screenshot (Win+Shift+S) and a picture copied from a browser.

`src/lua_callbacks.rs`

- A `Later`: the Lua state (`Rc<Lua>`, which keeps a state replaced by a
  config reload alive until its functions are done) and a `RegistryKey` for
  the function. `Later::call(self, args) -> Result<Vec<Command>, String>`
  builds a new `App` and calls the function with it and the args, as
  `call_action` does.
- `Command::Clipboard { kinds, then: Later }` and
  `Command::SendFile { bytes: Vec<u8>, extension: Option<String>, then: Later }`,
  pushed by `app:clipboard(kinds, f)` and `app:send_file(bytes, options, f)`.
  `bytes` is taken as a Lua string, which holds any bytes. A bad extension,
  or a kind not `text`, `image` or `files`, is an error in the action, raised
  at once.

`src/session.rs`

- `open` reads `Welcome` with `..` today; take `files` as well and keep it on
  `Daemon`.
- `Daemon` gets a table of who waits for each file, by request, like
  `snapshots` and `asks`. Request numbers come from `next_request`.
- `Link::send_file(self: &Arc<Self>, extension, bytes)`: only when the phase
  is `Attached`; registers the wait and sends `Frame::File`.
  `Link::keeps_files() -> bool` for the at-once refusal.
- The answer reaches the pane through a channel on the `Link` (a new one, or a
  new variant beside `Event::Shown` if that reads better), followed by
  `wake_pane()`. It carries the request and `Ok(path)` or `Err(why)`.
- In `read`: `Control::FileKept` answers `Ok`. In the `Control::Error` arm,
  look in the new table as well as `snapshots` and `starting`, and answer
  `Err`. Where the connection is lost (end of `read`), answer every waiting
  one with `Err("the connection to the session daemon was lost")`.

`src/terminal/pane.rs`

- The pane keeps what it waits for: each a `Later`, its request, and when it
  was asked.
- `Command::Clipboard` (in both places `Command` is matched:
  `finish_lua_action` about line 1585, and the picker callbacks about line
  2178): start a thread that calls `clipboard::read(kinds)` and sends the
  result back on a channel the pane holds, waking the pane.
- `Command::SendFile`: with no session link, or one whose daemon did not say
  `files`, or bytes over 32 MiB, answer `Err` at once (on the next frame,
  through the same path, so the function is always called later and never
  inside the action). Otherwise `link.send_file(..)`.
- Where the pane already polls its link each frame (`handle_pipe_requests`,
  called for every pane from `workspace_ui.rs` about line 2175), take the
  answers and call each `Later` with `(clip)` or `(path)`, or `(nil, why)`,
  then run what it asked through `finish_lua_action`.
- Give up on a file after 30 seconds with "the session daemon did not
  answer". Ask egui for a repaint at that time so the wait ends without a key
  press. A late answer for a request no longer waited for is dropped.
- A pane that closes drops what it waits for; the functions are not called.

`lua/nterm/clipboard.lua` (new): as in "The Lua API". Install it where
`lua/nterm/git.lua` is installed (`out\share\nterm\lua\nterm\`).

`nterm.example.lua`: the `PasteImage` action, with no key.

Tests

- A Lua action calling `app:send_file` gives the command with its bytes
  (including a zero byte) and extension; a bad extension is refused; the
  `Later` called with a path gives the commands its function asked for
  (`paste`); called with `nil, why`, a `show_toast`; the `app` an action was
  given still refuses use after it returned.
- `app:clipboard` with an unknown kind is refused; the `Later` given a
  `Clip` sees only the fields it holds.
- `clipboard.paste_image()` as a whole, with `clipboard` and `send_file`
  answered by the test: the path is pasted through `format`; with no picture,
  the toast.
- RGBA to PNG round trip, and the all-zero-alpha case. Reading the real
  clipboard is checked in the real run, not in tests: tests share it with
  whatever else runs.

## Run it for real

1. Build the window and the daemon. Both must be new: an old daemon ignores
   the frame.
2. `nterm --remote vm-self` (the loopback `ssh` set up for relay tests), with
   the `PasteImage` lines in `nterm.lua`.
3. Put a screenshot on the desktop's clipboard. An SSH login cannot reach the
   desktop clipboard, so use the `desk` tool or do it by hand.
4. Run "Paste picture from clipboard" from the command palette
   (Ctrl+Shift+P). The path appears at the prompt; open the file and look.
5. Repeat in a local window, with text on the clipboard, with an empty
   clipboard, and against a daemon built before this change (expect the
   "older than this window" message at once). An old daemon must use the
   same snapshot format, or the window refuses it for that first.
6. The `SendFile` picker and `PasteFiles` from "The Lua API", with a file of
   a few MB, and two files copied in Explorer.
7. Finally in `claude` in that pane: the picture attaches.

## Docs to update

- `docs/configuration.md`: the `app` methods (about lines 270 to 294), with
  a short part on functions nterm calls later, and `nterm.clipboard`.
- `docs/picker-pipe.md` about line 236 and `docs/copy-mode.md` about line 306
  list the `app` methods.
- `README.md`: one line in the features.

## Not in this piece

- Ctrl+V pasting the picture when the clipboard has no text, and any default
  key (decision 1). For whoever does it later: `egui-winit` turns Ctrl+V and
  Shift+Insert into `Event::Paste(text)` only when the clipboard has text;
  with none it swallows the key and nterm sees nothing (`egui-winit`
  `lib.rs`, `is_paste_command`), so a binding in `keys` can never see Ctrl+V.
  It needs a check in `src/app.rs` before `window.egui.on_window_event(..)`
  that asks only which formats the clipboard holds, not the picture itself.
- Clipboard kinds beyond `text`, `image` and `files` (`html`, or raw Windows
  formats by name).
- Dragging a file onto the window.
- Sending in pieces so typing does not wait behind a large file.
- Anything in `nterm-daemon.lua`: where the file goes, or a function run when
  one is kept.

## Working in this VM

- `out\bin` first on `PATH` before `cargo test`, or tests fail to start.
- Run daemon tests as
  `cargo test -p nterm-daemon -- --skip a_pseudoconsole_that_goes_on_its_own`,
  with output written to a file, not through a PowerShell pipe.
- Some tests already fail here for reasons unrelated to this work; compare
  with a run before the change, not with zero.
- Commit locally when a phase is done and tested. Do not push.
