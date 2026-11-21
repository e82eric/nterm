-- The session daemon's pickers on a machine with no nterm-daemon.lua of its
-- own: installed with the daemon in share/nterm/lua, it is read only when
-- there is none in the user's folder or beside the daemon. To change them,
-- start ~/nterm-daemon.lua from these, as the header of nterm/daemon.lua
-- shows, or name each picker yourself: see nterm-daemon.example.lua.
return require("nterm.daemon").defaults()
