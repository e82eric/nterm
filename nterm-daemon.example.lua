-- The pickers of this machine, for the session daemon.
--
-- The daemon reads the first there is of: the file `--lua-config` names,
-- ~/nterm-daemon.lua, and nterm-daemon.lua beside nterm-daemon.exe. It is
-- read when a picker of it is first asked for, and again when it has
-- changed: there is nothing to restart.
--
-- A picker here is written as one in nterm.lua is, and runs where the
-- daemon is: its program is run on this machine and its files are this
-- machine's. A window draws the rows, on this machine or another.
--
-- Open one by its name:
--   from a window, in nterm.lua:   Commits = { type = "Daemon", picker = "Commits" }
--   from a shell, in a profile:    Invoke-NtermPicker -DaemonPicker Commits
--
-- A program is only ever run because this file names it. What a picker is
-- opened with fills the arguments written as "{name}", each as one argument
-- whole, and is never which program is run.
--
-- So far: types Command, File, Items and FileSystem; previews of type
-- Dynamic, whose function runs here; and `action`, run here when a window
-- opened the picker, and `keys`, what they ask of `app` being done to the
-- pane. The names files, directories, history, ripgrep
-- and items are the daemon's own.
return {
  pickers = {
    -- The commits of the repository the shell is in.
    Commits = {
      type = "Command", sort = "SourceOrder",
      args = { "git", "log", "--oneline", "--decorate", "-n", "500" },
      -- What shows for a commit: the daemon runs the program this gives.
      preview = { type = "Dynamic", resolve = function(item, params)
        local hash = item.value:match("^(%x+)")
        if not hash then return nil end
        return { type = "Command", output = "Ansi", cwd = params.cwd,
                 args = { "git", "show", "--stat", "--patch", "--color=always", hash } }
      end },
      preview_visible = false,
      -- Keys of its own. One leaves the picker open unless its function
      -- closes it.
      keys = {
        { key = "y", mods = "CTRL", description = "Paste the commit's hash",
          action = function(app, item)
            app:paste(item.value:match("^(%x+)") or "")
            app:close_picker()
          end },
      },
    },
    -- The lines a pattern is found on, with flags of your own. Opened with
    -- a pattern: app:push_picker("Search", { pattern = item.value }).
    Search = {
      type = "Command", success_exit_codes = { 0, 1 },
      args = { "rg.exe", "--crlf", "--no-messages", "--color=never", "-i", "--vimgrep", "--", "{pattern}" },
      -- Enter, from a window: the line is opened in an editor in the shell.
      action = function(app, item)
        local path, line = item.value:match("^(.*):(%d+):%d+:")
        if not path then return end
        app:send_text("nvim +" .. line .. " '" .. path:gsub("'", "''") .. "'\r")
      end,
    },
    -- What was typed at a shell, the last first. Opened with the file's
    -- path as `history`.
    Typed = {
      type = "File", path = "{history}", reverse = true,
      line_continuation = "`", unique = true, sort = "SourceOrder", position = "Cursor",
    },
    -- The files under the folder the shell is in, no deeper than four.
    Near = {
      type = "FileSystem", max_depth = 4, files_only = true, relative = true,
      position = "Cursor", preview_visible = false,
    },
    -- Rows written here.
    Servers = {
      type = "Items",
      items = {
        { text = "build   the build machine", value = "ssh build" },
        { text = "db      the database", value = "ssh db" },
      },
    },
  },
}
