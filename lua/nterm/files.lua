-- Pickers of files and folders for nterm: what is under the folders a
-- picker is opened with as `roots` (a JSON list of whole paths), or else
-- under those the file gives, or else under the shell's directory.
--
--   -- nterm-daemon.lua, on the machine the shell is on
--   local files = require("nterm.files")
--   return {
--     pickers = {
--       Files = files.files(),
--       Directories = files.directories { roots = { "C:\\src" } },
--       Everything = files.all { roots = { "C:\\" }, preview = files.preview() },
--     },
--   }
--
--   # From PowerShell, in a pane of that machine
--   Invoke-NtermPicker -DaemonPicker Files -Parameters @{ cwd = $PWD.Path }
--
-- A row is the whole path of a file or folder. Without a preview of its
-- own, the daemon shows it as the start of the file, a picture as itself,
-- and a folder as what is in it; files.preview() shows more (see there). In
-- nterm.lua they are the window's own pickers of this machine's files.
--
-- Options: roots, max_depth (how many levels down; all with none), relative
-- (rows said from the one folder looked under), and the fields any picker
-- has, which are passed on as they are: position, preview_visible (false
-- unless given), sort, initial_query, action, keys, preview.
local M = {}

local passed = {
  "roots", "max_depth", "relative",
  "position", "sort", "initial_query", "action", "keys", "preview",
}

local function picker(options, kind)
  options = options or {}
  local made = { type = "FileSystem", preview_visible = options.preview_visible or false }
  if kind then
    made[kind] = true
  end
  for _, name in ipairs(passed) do
    made[name] = options[name]
  end
  return made
end

-- The files, and not the folders.
function M.files(options)
  return picker(options, "files_only")
end

-- The folders, and not the files.
function M.directories(options)
  return picker(options, "directories_only")
end

-- The files and the folders.
function M.all(options)
  return picker(options, nil)
end

-- The files that show as pictures, and those that show as a frame of them,
-- by how their names end.
M.images ={ png = true, jpg = true, jpeg = true, gif = true, bmp = true, webp = true, tif = true, tiff = true }
M.videos = { mp4 = true, mkv = true, mov = true, avi = true, webm = true, m4v = true, wmv = true }

-- The most of a file that is shown.
local MOST = 10 * 1024 * 1024

local function whole(path, cwd)
  if not cwd or path:match("^%a:") or path:match("^[\\/]") then
    return path
  end
  return cwd .. "/" .. path
end

local function run(args, cwd)
  local result = preview.run { args = args, cwd = cwd, timeout_ms = 10000 }
  assert(result.exit_code == 0, result.stderr)
  return result.stdout
end

local function read_bytes(path)
  local file = assert(io.open(path, "rb"))
  local ok, bytes = pcall(function()
    local bytes, read_error = file:read(MOST + 1)
    assert(not read_error, read_error)
    bytes = bytes or ""
    assert(#bytes <= MOST, "File preview exceeds 10 MiB")
    return bytes
  end)
  file:close()
  assert(ok, bytes)
  return bytes
end

-- How many bytes the UTF-8 character that starts with `lead` has, or nil.
local function width_of(lead)
  return lead >= 194 and lead <= 223 and 2
    or lead >= 224 and lead <= 239 and 3
    or lead >= 240 and lead <= 244 and 4
    or nil
end

-- The text of the file, and whether it is not text: then what is said is
-- its size.
local function read_text(path, shown_as)
  local file = assert(io.open(path, "rb"))
  local ok, result = pcall(function()
    local function read(count)
      local bytes, read_error = file:read(count)
      assert(not read_error, read_error)
      return bytes or ""
    end
    local function binary(bytes)
      return bytes:find(string.char(0), 1, true) ~= nil or utf8.len(bytes) == nil
    end
    local function size()
      local bytes = assert(file:seek("end"))
      return { text = "Binary file\n" .. shown_as .. "\n" .. bytes .. " bytes", binary = true }
    end
    local sample = read(4096)
    -- A character the sample cuts in two is finished before it is judged.
    local length, invalid = utf8.len(sample)
    if not length and invalid > #sample - 3 then
      local width = width_of(sample:byte(invalid))
      if width and #sample - invalid + 1 < width then
        sample = sample .. read(width - (#sample - invalid + 1))
      end
    end
    if binary(sample) then
      return size()
    end
    assert(file:seek("set", 0))
    local bytes = read(MOST + 1)
    local truncated = #bytes > MOST
    local warning = "\n\n[Preview truncated: file exceeds the 10 MiB text limit.]\n"
    if truncated then
      bytes = bytes:sub(1, MOST - #warning)
      -- Not half a character at the end.
      local valid, position = utf8.len(bytes)
      if not valid and position > #bytes - 3 then
        local width = width_of(bytes:byte(position))
        if width and #bytes - position + 1 < width then
          bytes = bytes:sub(1, position - 1)
        end
      end
    end
    if binary(bytes) then
      return size()
    end
    local text = bytes:gsub("^\239\187\191", "")
    if truncated then
      text = text .. warning
    end
    return { text = text, binary = false }
  end)
  file:close()
  assert(ok, result)
  return result.text, result.binary
end

local function folder(path)
  local items = preview.directory_items(path)
  if not items then
    return nil
  end
  local lines = {}
  for _, item in ipairs(items) do
    lines[#lines + 1] = item.name .. (item.is_directory and "/" or "")
  end
  return #lines > 0 and table.concat(lines, "\n") or "[Empty directory]"
end

-- What shows for a row of these, said in Lua: a folder as what is in it; a
-- picture as itself, up to 10 MiB; a video as a frame of it, a quarter of
-- the way in, which ffprobe and ffmpeg make, so they have to be on the PATH
-- of what runs the picker; a file of text as its text, up to 10 MiB, and
-- any other file as its size. It runs those programs, and nothing else.
--
-- Options: ffmpeg ("ffmpeg.exe"), ffprobe ("ffprobe.exe"), video_position
-- (0.25, how far into a video), theme (the colours of text, where the
-- window uses it: a window drawing a daemon's picker uses its own).
function M.preview(options)
  options = options or {}
  local ffmpeg = options.ffmpeg or "ffmpeg.exe"
  local ffprobe = options.ffprobe or "ffprobe.exe"
  local position = options.video_position or 0.25
  local theme = options.theme
  return {
    type = "Dynamic",
    resolve = function(item, params)
      local path = item.value
      local at = whole(path, params.cwd)
      local extension = (path:match("%.([^./\\]+)$") or ""):lower()
      -- A folder cannot be opened as a file on Windows. On Linux it can,
      -- and then cannot be read: an empty file is read, as nothing.
      local probe = io.open(at, "rb")
      local listed
      if probe then
        local _, unreadable = probe:read(0)
        probe:close()
        if unreadable then
          listed = folder(at)
        end
      else
        listed = folder(at)
      end
      if listed then
        return { type = "Text", text = listed, syntax = "PlainText", theme = theme }
      end
      if M.images[extension] then
        return { type = "Image", bytes = read_bytes(at) }
      end
      if M.videos[extension] then
        local duration = tonumber(run({ ffprobe, "-v", "error", "-show_entries", "format=duration",
          "-of", "default=noprint_wrappers=1:nokey=1", path }, params.cwd))
        assert(duration and duration >= 0 and duration < math.huge, "Invalid video duration")
        local seek = string.format("%.3f", duration * position)
        local bytes = run({ ffmpeg, "-loglevel", "error", "-ss", seek, "-i", path, "-frames:v", "1",
          "-vf", "scale=1280:1280:force_original_aspect_ratio=decrease",
          "-f", "image2pipe", "-vcodec", "png", "pipe:1" }, params.cwd)
        return { type = "Image", bytes = bytes }
      end
      local text, binary = read_text(at, path)
      if binary then
        return { type = "Text", text = text, syntax = "PlainText", theme = theme }
      end
      return { type = "Text", text = text, syntax = "Auto", extension = extension, name = path, theme = theme }
    end,
  }
end

return M
