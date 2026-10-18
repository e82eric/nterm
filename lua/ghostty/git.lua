-- Git pickers for ghostty-egui: commits and branches of the repository in the
-- shell's current directory. They run git directly, without a shell, so they
-- work the same on Linux and Windows; the branch switch command is typed into
-- bash or PowerShell 7 with that shell's quoting.
--
--   local git = require("ghostty.git")
--   return {
--     pickers = {
--       ["Git commits"] = git.commits(),
--       ["Git branches"] = git.branches({ prompt = "^%[[^%]]*%][$#] " }),
--     },
--   }
local M = {}

local windows = package.config:sub(1, 1) == "\\"

-- Shell quoting for text typed at a prompt. Branch names come from remotes and
-- may contain characters a shell interprets, such as $( ) ; | and quotes.
M.quote = {}

-- POSIX shells (bash, zsh): bare when only safe characters are used,
-- otherwise single-quoted with each ' written as '\''.
function M.quote.posix(text)
  if text:match("^[%w%._/%-+=:,@]+$") then
    return text
  end
  return "'" .. text:gsub("'", [['\'']]) .. "'"
end

-- PowerShell: always single-quoted. Every character PowerShell treats as a
-- single quote, including the typographic ones, is doubled.
function M.quote.powershell(text)
  for _, quote in ipairs({ "'", "\u{2018}", "\u{2019}", "\u{201A}", "\u{201B}" }) do
    text = text:gsub(quote, quote .. quote)
  end
  return "'" .. text .. "'"
end

local default_quote = windows and M.quote.powershell or M.quote.posix

-- How many characters to erase before and after the cursor when the cursor
-- line matches the prompt `pattern`, or nil when it does not (e.g. in vim).
local function prompt_input(context, pattern)
  if not pattern then
    return nil
  end
  local before = context.line_before_cursor
  if not before:find(pattern) then
    return nil
  end
  local typed = before:gsub(pattern, "", 1)
  local after = context.line:sub(#before + 1):gsub("%s+$", "")
  return math.min(utf8.len(typed) or #typed, 4096), math.min(utf8.len(after) or #after, 4096)
end

local function commit_hash(line)
  return line:match("^(%x+)\t")
end

-- Commits of every branch, remote branch and tag, newest first, with the
-- branch each was reached from. Enter inserts the abbreviated hash.
-- Options: position ("Cursor"), preview_visible (false; Ctrl+P toggles it).
function M.commits(options)
  options = options or {}
  return {
    type = "Command",
    -- With these ref selections, --source prints short names in %S
    -- (main, origin/main); --all would print refs/heads/main.
    args = {
      "git", "log", "--branches", "--remotes", "--tags", "HEAD", "--source",
      "--date-order", "--date=short", "--color=never",
      "--format=%h%x09%ad%x09%S%x09%an%x09%s",
    },
    columns = { "Hash", "Date", "Branch", "Author", "Subject" },
    -- Keep git's newest-first order while filtering.
    sort = "Source",
    position = options.position or "Cursor",
    preview_visible = options.preview_visible or false,
    preview = {
      type = "Dynamic",
      resolve = function(item, params)
        local hash = commit_hash(item.value)
        if not hash then
          return nil
        end
        return {
          type = "Command", output = "Ansi", cwd = params.cwd,
          args = { "git", "show", "--stat", "--patch", "--color=always", hash },
        }
      end,
    },
    action = function(app, item)
      local hash = commit_hash(item.value)
      if hash then
        app:send_text(hash)
      end
    end,
  }
end

-- One row per branch; the symbolic origin/HEAD prints as an empty line, which
-- the picker skips.
local branch_format = "--format=%(if)%(symref)%(then)%(else)%(HEAD)%09%(refname:short)%09"
  .. "%(if:equals=refs/remotes)%(refname:rstrip=-2)%(then)remote%(else)local%(end)%09"
  .. "%(committerdate:short)%09%(authorname)%09%(subject)%(end)"

local function branch_row(line)
  local head, branch, where = line:match("^([^\t]*)\t([^\t]+)\t([^\t]+)\t")
  if not branch then
    return nil
  end
  return { branch = branch, remote = where == "remote", current = head == "*" }
end

-- The command that switches to the branch on `line` (a row of the branches
-- picker), or nil. A remote row switches to the name without its remote
-- (origin/feature/x -> feature/x): git switch then uses the existing local
-- branch or creates one tracking the remote. Remote names containing "/" are
-- not supported. Options: quote (shell quoting function), require_clean.
function M.switch_command(line, options)
  options = options or {}
  local row = branch_row(line)
  if not row then
    return nil
  end
  local target = row.branch
  if row.remote then
    target = target:match("^[^/]+/(.+)$") or target
  end
  local command = "git switch " .. (options.quote or default_quote)(target)
  if options.require_clean then
    -- git diff lists changed tracked files and fails when there are any, so
    -- the switch only runs from a clean tree. bash and PowerShell 7 support &&.
    command = "git diff --stat --exit-code HEAD && " .. command
  end
  return command
end

-- Ctrl+P: how far the branch is ahead of or behind the current one, and its
-- recent history. Runs git directly, so branch names never reach a shell.
local function branch_preview(item, params)
  local row = branch_row(item.value)
  if not row then
    return nil
  end
  local function git(...)
    return preview.run({ args = { "git", ... }, cwd = params.cwd })
  end
  local current = git("branch", "--show-current")
  local here = current.exit_code == 0 and current.stdout:match("^%s*(.-)%s*$") or ""
  if here == "" then
    here = "HEAD"
  end
  local header = ""
  local counts = git("rev-list", "--left-right", "--count", "HEAD..." .. row.branch)
  local behind, ahead = counts.stdout:match("(%d+)%s+(%d+)")
  if counts.exit_code == 0 and behind then
    header = string.format("\27[1m%s\27[0m: %s ahead, %s behind %s\n\n", row.branch, ahead, behind, here)
  end
  local log = git(
    "log", "--graph", "--oneline", "--decorate", "--color=always", "-30",
    "--end-of-options", row.branch, "--"
  )
  if log.exit_code ~= 0 then
    return { type = "Text", text = log.stderr }
  end
  return { type = "Ansi", bytes = header .. log.stdout }
end

-- Local and remote branches, most recently updated first; * marks the current
-- one. At a prompt matching options.prompt, Enter erases what is typed and
-- runs the switch command (see switch_command); elsewhere it inserts the
-- branch name. Options: prompt (Lua pattern for the shell prompt), quote,
-- require_clean, position ("Cursor"), preview_visible (false).
function M.branches(options)
  options = options or {}
  return function(context)
    local erase_before, erase_after = prompt_input(context, options.prompt)
    return {
      type = "Command",
      args = {
        "git", "for-each-ref", "--sort=-committerdate", "refs/heads", "refs/remotes",
        branch_format,
      },
      columns = { "HEAD", "Branch", "Where", "Date", "Author", "Last commit" },
      sort = "Source",
      position = options.position or "Cursor",
      preview_visible = options.preview_visible or false,
      preview = { type = "Dynamic", resolve = branch_preview },
      action = function(app, item)
        local row = branch_row(item.value)
        if not row then
          return
        end
        if erase_before then
          -- send_text presses a real Enter; replace_prompt's text is a paste.
          app:replace_prompt("", erase_before, erase_after)
          app:send_text(M.switch_command(item.value, options) .. "\n")
        else
          app:send_text(row.branch)
        end
      end,
    }
  end
end

return M
