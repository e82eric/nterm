-- Pasting what is on this machine's clipboard into a shell that may be on
-- another machine: a picture is sent there as a file, and its path typed.
--
--   local clipboard = require("nterm.clipboard")
--   return {
--     actions = {
--       PasteImage = { label = "Paste picture from clipboard", action = clipboard.paste_image() },
--     },
--   }
--
-- Built of two methods of `app`, which a config can use as it likes:
--
--   app:clipboard({ "text", "image", "files" }, function(app, clip, why) end)
--     What the clipboard holds of the kinds asked for: clip.text, clip.image
--     (a PNG file's bytes) and clip.files (full paths on this machine), each
--     there only if the clipboard has it. On failure, nil and why.
--
--   app:send_file(bytes, { extension = "png" }, function(app, path, why) end)
--     The bytes kept as a file on the machine the pane's shell is on, by the
--     session daemon there; path is where. On failure, nil and why.
--
-- Both return at once. The function is called on a later frame, with a new
-- `app` for the same pane, and not at all if the pane has gone by then.
local M = {}

-- What Claude Code takes as a picture when the path is pasted at its prompt.
function M.claude_path(path)
  return path
end

-- An action: the clipboard's picture, sent to the machine the pane's shell
-- is on, and its path pasted at the prompt. `options.format(path)` gives
-- what is pasted; M.claude_path without it.
function M.paste_image(options)
  local format = (options or {}).format or M.claude_path
  return function(app)
    app:clipboard({ "image" }, function(app, clip, why)
      if not clip then
        return app:show_toast("Paste picture", why, 3000)
      end
      if not clip.image then
        return app:show_toast("Paste picture", "The clipboard holds no picture.", 3000)
      end
      app:send_file(clip.image, { extension = "png" }, function(app, path, why)
        if not path then
          return app:show_toast("Could not paste picture", why, 5000)
        end
        app:paste(format(path))
      end)
    end)
  end
end

return M
