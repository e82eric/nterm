-- Git pickers for ghostty-egui, for the repository in the shell's current
-- directory. They run git directly, without a shell, so they work the same on
-- Linux and Windows; commands typed at the prompt use bash or PowerShell 7
-- quoting.
--
--   local git = require("ghostty.git")
--   local prompt = "^%[[^%]]*%][$#] "
--   return {
--     pickers = {
--       ["Git commits"] = git.commits(),
--       ["Git branches"] = git.branches({ prompt = prompt }),
--       ["Git changed files"] = git.status({ prompt = prompt }),
--       ["Git cherry-pick"] = git.cherry_pick(),
--       ["Git cherry-pick commits"] = git.cherry_pick_commits({ prompt = prompt }),
--       ["Git stashes"] = git.stashes({ prompt = prompt }),
--       ["Git reflog"] = git.reflog(),
--       ["Git file history"] = git.file_history({ prompt = prompt }),
--       ["Git file history commits"] = git.file_history_commits(),
--       ["Git fixup"] = git.fixups({ prompt = prompt }),
--       ["Git tags"] = git.tags({ prompt = prompt }),
--     },
--   }
--
-- Common options: position ("Cursor"), preview_visible (false; Ctrl+P toggles
-- it), prompt (a Lua pattern matching the shell prompt), quote (shell quoting
-- function, see M.quote) and run (press Enter after a typed command).
local M = {}

local windows = package.config:sub(1, 1) == "\\"

-- Shell quoting for text typed at a prompt. Branch, tag and file names may
-- contain characters a shell interprets, such as $( ) ; | and quotes.
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

local function quote_of(options)
  return options.quote or default_quote
end

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

-- The prompt state for an action: erase counts at a matching prompt, or nil.
local function prompt_state(context, pattern)
  local before, after = prompt_input(context, pattern)
  return before and { before, after } or nil
end

-- At a prompt, replace what is typed with `command`, pressing Enter when
-- `run` is set; elsewhere insert `fallback`.
local function type_or_insert(app, prompt, command, run, fallback)
  if prompt then
    -- send_text presses a real Enter; replace_prompt's text is a paste.
    app:replace_prompt("", prompt[1], prompt[2])
    app:send_text(command .. (run and "\n" or ""))
  else
    app:send_text(fallback)
  end
end

-- Insert a name, quoted for the shell when at a prompt.
local function insert_name(app, prompt, name, options)
  app:send_text(prompt and quote_of(options)(name) or name)
end

local function picker_options(options, picker)
  picker.sort = "Source"
  picker.position = options.position or "Cursor"
  picker.preview_visible = options.preview_visible or false
  return picker
end

local function commit_hash(line)
  return line:match("^(%x+)\t")
end

-- Ctrl+P for a row starting with a commit hash: `git show` of that commit.
local show_preview = {
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
}

-- A `git log` picker with Hash / Date / Author / Subject columns, newest
-- first. `extra` follows the fixed arguments. Enter types `command <hash>` at
-- a prompt when `command` is given, and otherwise inserts the hash.
local function log_picker(extra, options, prompt, command)
  local args = { "git", "log", "--date=short", "--color=never", "--format=%h%x09%ad%x09%an%x09%s" }
  for _, arg in ipairs(extra) do
    args[#args + 1] = arg
  end
  return picker_options(options, {
    type = "Command",
    args = args,
    columns = { "Hash", "Date", "Author", "Subject" },
    preview = show_preview,
    action = function(app, item)
      local hash = commit_hash(item.value)
      if not hash then
        return
      end
      if command then
        type_or_insert(app, prompt, command .. " " .. hash, options.run, hash)
      else
        app:send_text(hash)
      end
    end,
  })
end

-- Commits of every branch, remote branch and tag, newest first, with the
-- branch each was reached from. Enter inserts the abbreviated hash.
function M.commits(options)
  options = options or {}
  return picker_options(options, {
    type = "Command",
    -- With these ref selections, --source prints short names in %S
    -- (main, origin/main); --all would print refs/heads/main.
    args = {
      "git", "log", "--branches", "--remotes", "--tags", "HEAD", "--source",
      "--date-order", "--date=short", "--color=never",
      "--format=%h%x09%ad%x09%S%x09%an%x09%s",
    },
    columns = { "Hash", "Date", "Branch", "Author", "Subject" },
    preview = show_preview,
    action = function(app, item)
      local hash = commit_hash(item.value)
      if hash then
        app:send_text(hash)
      end
    end,
  })
end

-- One row per branch; the symbolic origin/HEAD prints as an empty line, which
-- the picker skips.
local branch_format = "--format=%(if)%(symref)%(then)%(else)%(HEAD)%09%(refname:short)%09"
  .. "%(if:equals=refs/remotes)%(refname:rstrip=-2)%(then)remote%(else)local%(end)%09"
  .. "%(committerdate:short)%09%(authorname)%09%(subject)%(end)"
local branch_args = {
  "git", "for-each-ref", "--sort=-committerdate", "refs/heads", "refs/remotes", branch_format,
}
local branch_columns = { "HEAD", "Branch", "Where", "Date", "Author", "Last commit" }

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
-- not supported. Options: quote, require_clean.
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
  local command = "git switch " .. quote_of(options)(target)
  if options.require_clean then
    -- git diff lists changed tracked files and fails when there are any, so
    -- the switch only runs from a clean tree. bash and PowerShell 7 support &&.
    command = "git diff --stat --exit-code HEAD && " .. command
  end
  return command
end

-- Ctrl+P for a branch row: how far it is ahead of or behind the current
-- branch, and its recent history. Branch names never reach a shell.
local branch_preview = {
  type = "Dynamic",
  resolve = function(item, params)
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
  end,
}

-- Local and remote branches, most recently updated first; * marks the current
-- one. At a prompt Enter erases what is typed and runs the switch command (see
-- switch_command; run = false only types it); elsewhere it inserts the branch
-- name. Options: require_clean, plus the common options.
function M.branches(options)
  options = options or {}
  return function(context)
    local prompt = prompt_state(context, options.prompt)
    return picker_options(options, {
      type = "Command",
      args = branch_args,
      columns = branch_columns,
      preview = branch_preview,
      action = function(app, item)
        local row = branch_row(item.value)
        if row then
          type_or_insert(app, prompt, M.switch_command(item.value, options), options.run ~= false, row.branch)
        end
      end,
    })
  end
end

local status_escapes = {
  a = "\a", b = "\b", f = "\f", n = "\n", r = "\r", t = "\t", v = "\v", ['"'] = '"', ["\\"] = "\\",
}

-- Undo git's C-style quoting of a path ("a\tb", octal \303\251).
function M.unquote_path(text)
  if #text < 2 or text:sub(1, 1) ~= '"' or text:sub(-1) ~= '"' then
    return text
  end
  local out, i = {}, 2
  while i < #text do
    local c = text:sub(i, i)
    if c == "\\" then
      local octal = text:match("^[0-7][0-7][0-7]", i + 1)
      if octal then
        out[#out + 1] = string.char(tonumber(octal, 8))
        i = i + 4
      else
        local next = text:sub(i + 1, i + 1)
        out[#out + 1] = status_escapes[next] or next
        i = i + 2
      end
    else
      out[#out + 1] = c
      i = i + 1
    end
  end
  return table.concat(out)
end

-- The status code and path of a `git status --short` line. Renames and copies
-- ("R  old -> new") give the new path.
function M.status_entry(line)
  local code, path = line:match("^(..) (.+)$")
  if not code then
    return nil
  end
  if code:find("[RC]") then
    path = path:match(" %-> (.+)$") or path
  end
  return code, M.unquote_path(path)
end

-- Ctrl+P for a changed file: staged and unstaged diffs, or the start of an
-- untracked file.
local status_preview = {
  type = "Dynamic",
  resolve = function(item, params)
    local code, path = M.status_entry(item.value)
    if not code then
      return nil
    end
    if code == "??" then
      local file = io.open(params.cwd and (params.cwd .. "/" .. path) or path, "rb")
      if not file then
        return { type = "Text", text = "Untracked: " .. path }
      end
      local text = file:read(256 * 1024) or ""
      file:close()
      if text:find("\0", 1, true) then
        return { type = "Text", text = "Untracked binary file: " .. path }
      end
      return { type = "Text", text = text, name = path, syntax = "Auto" }
    end
    local function diff(staged)
      local args = { "git", "diff", "--color=always" }
      if staged then
        args[#args + 1] = "--cached"
      end
      args[#args + 1] = "--"
      args[#args + 1] = path
      return preview.run({ args = args, cwd = params.cwd })
    end
    local parts = {}
    for _, section in ipairs({ { true, "Staged" }, { false, "Not staged" } }) do
      local result = diff(section[1])
      if result.exit_code == 0 and #result.stdout > 0 then
        parts[#parts + 1] = "\27[1m" .. section[2] .. "\27[0m\n" .. result.stdout
      end
    end
    if #parts == 0 then
      return { type = "Text", text = "No diff for " .. path }
    end
    return { type = "Ansi", bytes = table.concat(parts, "\n") }
  end,
}

-- Changed files, as `git status --short` lines with paths relative to the
-- shell's directory: staged, unstaged, untracked and conflicted. Enter inserts
-- the path (quoted at a prompt).
function M.status(options)
  options = options or {}
  return function(context)
    local prompt = prompt_state(context, options.prompt)
    return picker_options(options, {
      type = "Command",
      args = {
        "git", "-c", "color.status=false", "-c", "core.quotePath=false",
        "status", "--short", "--untracked-files=all",
      },
      preview = status_preview,
      action = function(app, item)
        local _, path = M.status_entry(item.value)
        if path then
          insert_name(app, prompt, path, options)
        end
      end,
    })
  end
end

-- Step 1 of cherry-picking: choose a branch. Enter opens the commits picker
-- named options.commits ("Git cherry-pick commits") with that branch's commits
-- that are not in the current branch.
function M.cherry_pick(options)
  options = options or {}
  local commits = options.commits or "Git cherry-pick commits"
  return picker_options(options, {
    type = "Command",
    args = branch_args,
    columns = branch_columns,
    preview = branch_preview,
    action = function(app, item)
      local row = branch_row(item.value)
      if row then
        app:replace_picker(commits, { range = "HEAD..." .. row.branch })
      end
    end,
  })
end

-- Step 2 of cherry-picking, opened by M.cherry_pick with a `range` parameter:
-- commits of the chosen branch that are not in the current one, without merges
-- and without commits already applied here (--cherry-pick). Enter types
-- `git cherry-pick <hash>` at a prompt.
function M.cherry_pick_commits(options)
  options = options or {}
  return function(context)
    return log_picker(
      { "--cherry-pick", "--right-only", "--no-merges", "--end-of-options", "{range}" },
      options,
      prompt_state(context, options.prompt),
      "git cherry-pick"
    )
  end
end

-- Stashes, newest first. Enter types `git stash apply <ref>` at a prompt
-- (apply keeps the stash, unlike pop); elsewhere it inserts the ref.
function M.stashes(options)
  options = options or {}
  return function(context)
    local prompt = prompt_state(context, options.prompt)
    return picker_options(options, {
      type = "Command",
      -- No --date: it would turn stash@{0} into stash@{<date>}.
      args = { "git", "stash", "list", "--color=never", "--format=%gd%x09%cs%x09%gs" },
      columns = { "Ref", "Date", "Message" },
      preview = {
        type = "Dynamic",
        resolve = function(item, params)
          local ref = item.value:match("^([^\t]+)\t")
          if not ref then
            return nil
          end
          return {
            type = "Command", output = "Ansi", cwd = params.cwd,
            args = { "git", "stash", "show", "--patch", "--color=always", ref },
          }
        end,
      },
      action = function(app, item)
        local ref = item.value:match("^([^\t]+)\t")
        if ref then
          type_or_insert(app, prompt, "git stash apply " .. quote_of(options)(ref), options.run, ref)
        end
      end,
    })
  end
end

-- Where HEAD has been, newest first, including commits no branch points to
-- any more (e.g. after a reset or rebase). Enter inserts the hash.
function M.reflog(options)
  options = options or {}
  return picker_options(options, {
    type = "Command",
    -- No --date: it would turn HEAD@{0} into HEAD@{<date>}.
    args = { "git", "log", "--walk-reflogs", "--color=never", "--format=%h%x09%gd%x09%cs%x09%gs" },
    columns = { "Hash", "Entry", "Date", "Action" },
    preview = show_preview,
    action = function(app, item)
      local hash = commit_hash(item.value)
      if hash then
        app:send_text(hash)
      end
    end,
  })
end

-- The word around the cursor, without the prompt and surrounding quotes.
local function word_at_cursor(context, pattern)
  local before = context.line_before_cursor
  if pattern then
    before = before:gsub(pattern, "", 1)
  end
  local word = (before:match("(%S+)$") or "") .. (context.line:sub(#context.line_before_cursor + 1):match("^%S*") or "")
  return word:match([[^'(.*)'$]]) or word:match([[^"(.*)"$]]) or word
end

-- Step 1 of file history: choose a tracked file under the shell's directory,
-- starting with the word at the cursor as the query. Ctrl+P shows the file's
-- recent commits. Enter opens the picker named options.commits ("Git file
-- history commits") for that file.
function M.file_history(options)
  options = options or {}
  local commits = options.commits or "Git file history commits"
  return function(context)
    local word = word_at_cursor(context, options.prompt)
    return picker_options(options, {
      type = "Command",
      args = { "git", "-c", "core.quotePath=false", "ls-files" },
      initial_query = word ~= "" and word or nil,
      preview = {
        type = "Dynamic",
        resolve = function(item, params)
          local path = M.unquote_path(item.value)
          local log = preview.run({
            args = { "git", "log", "--oneline", "--color=always", "-15", "--follow", "--", path },
            cwd = params.cwd,
          })
          if log.exit_code ~= 0 then
            return { type = "Text", text = log.stderr }
          end
          return { type = "Ansi", bytes = log.stdout }
        end,
      },
      action = function(app, item)
        app:replace_picker(commits, { path = M.unquote_path(item.value) })
      end,
    })
  end
end

-- Step 2 of file history, opened by M.file_history with a `path` parameter:
-- commits that changed the file, following renames. Ctrl+P shows that
-- commit's change to the file. Enter inserts the hash.
function M.file_history_commits(options)
  options = options or {}
  local picker = log_picker({ "--follow", "--", "{path}" }, options, nil, nil)
  picker.preview = {
    type = "Dynamic",
    resolve = function(item, params)
      local hash = commit_hash(item.value)
      if not hash or not params.path then
        return nil
      end
      return {
        type = "Command", output = "Ansi", cwd = params.cwd,
        args = { "git", "log", "-1", "--patch", "--follow", "--color=always", hash, "--", params.path },
      }
    end,
  }
  return picker
end

-- Commits on the current branch that are not on its upstream (or on
-- options.base), as targets for `git commit --fixup`. Enter types that at a
-- prompt; follow with `git rebase -i --autosquash`.
function M.fixups(options)
  options = options or {}
  return function(context)
    local range = (options.base and (options.base .. "..HEAD")) or "@{upstream}..HEAD"
    return log_picker({ "--end-of-options", range }, options, prompt_state(context, options.prompt), "git commit --fixup")
  end
end

-- Tags, newest first. Enter inserts the tag name (quoted at a prompt).
function M.tags(options)
  options = options or {}
  return function(context)
    local prompt = prompt_state(context, options.prompt)
    return picker_options(options, {
      type = "Command",
      args = {
        "git", "for-each-ref", "--sort=-creatordate", "refs/tags",
        "--format=%(refname:short)%09%(creatordate:short)%09%(subject)",
      },
      columns = { "Tag", "Date", "Subject" },
      preview = {
        type = "Dynamic",
        resolve = function(item, params)
          local tag = item.value:match("^([^\t]+)\t")
          if not tag then
            return nil
          end
          return {
            type = "Command", output = "Ansi", cwd = params.cwd,
            args = { "git", "show", "--stat", "--color=always", "--end-of-options", tag, "--" },
          }
        end,
      },
      action = function(app, item)
        local tag = item.value:match("^([^\t]+)\t")
        if tag then
          insert_name(app, prompt, tag, options)
        end
      end,
    })
  end
end

return M
