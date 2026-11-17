//! Sessions: a workspace's shells and its layout, kept in the session daemon.
//!
//! Shells that run in the daemon outlive the window that showed them. Each
//! workspace is one session: it describes itself in terms of the daemon's
//! panes, and the description is kept in the daemon whenever it changes.
//! Closing a window, or detaching a workspace, leaves its session waiting;
//! any window can take a waiting session back as a workspace. A window is
//! only a view and nothing about it is kept.
//!
//! Everything here runs on the UI thread.

use super::{
    FloatingTerminal, TerminalView,
    workspace::{self, Appearance, Layout},
};
use crate::{actions::AppAction, graphics as glow};
use nterm_session::protocol::{PaneInfo, SessionInfo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Bumped when a description stops meaning what it did.
const VERSION: u32 = 1;

/// One workspace, by the daemon's pane numbers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    version: u32,
    layout: Layout,
    main_ratio: Option<f32>,
    panes: Vec<u32>,
    focused: Option<u32>,
    weights: BTreeMap<Layout, BTreeMap<u32, f32>>,
    appearance: Option<Appearance>,
    floats: Vec<Float>,
    visible_float: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Float {
    pane: u32,
    name: Option<String>,
}

impl Saved {
    /// Every shell the workspace holds, tiled and floating.
    fn members(&self) -> Vec<u32> {
        self.panes.iter().copied().chain(self.floats.iter().map(|float| float.pane)).collect()
    }

    /// Cut down to the shells `keep` accepts. Floats belong to a workspace,
    /// which needs a tiled pane: left without one, they are tiled.
    fn keep_only(&mut self, mut keep: impl FnMut(u32) -> bool) {
        self.panes.retain(|pane| keep(*pane));
        self.floats.retain(|float| keep(float.pane));
        if self.panes.is_empty() {
            self.panes = self.floats.drain(..).map(|float| float.pane).collect();
            self.visible_float = None;
        }
    }
}

/// A new session's name in the daemon. It only has to differ from the
/// others there.
fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{time:x}-{:x}-{:x}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The shells of a session that can be taken: all of them, when nothing
/// shows any. A session is taken whole, so one a window shows part of is
/// not offered. The shells in no session, which the daemon gives under no
/// name, are not one thing, and those of them that wait are offered.
fn takeable(session: &SessionInfo) -> Vec<&PaneInfo> {
    let waiting: Vec<&PaneInfo> =
        session.panes.iter().filter(|pane| crate::session::waiting(pane)).collect();
    if session.id.is_empty() || waiting.len() == session.panes.len() {
        waiting
    } else {
        Vec::new()
    }
}

/// A picker's rows for the sessions the daemon holds that nothing is
/// showing: what runs in each, how many panes and where.
pub(super) fn session_rows(listing: &crate::session::Listing) -> Vec<crate::actions::Entry> {
    let name = |pane: &PaneInfo| {
        let title = pane.title.as_deref().filter(|title| !title.trim().is_empty());
        let name = title.unwrap_or(&pane.command);
        // A path says less than the program it ends in.
        let short = name.rsplit(['\\', '/']).next().filter(|short| !short.is_empty());
        short.unwrap_or(name).to_owned()
    };
    let row = |session: &SessionInfo| {
        let panes = takeable(session);
        if panes.is_empty() {
            return None;
        }
        let mut names: Vec<String> = panes.iter().take(3).map(|pane| name(pane)).collect();
        if panes.len() > names.len() {
            names.push(format!("+{}", panes.len() - names.len()));
        }
        let count = match panes.len() {
            1 => "1 pane".to_owned(),
            count => format!("{count} panes"),
        };
        let kind = if session.id.is_empty() { "Shells in no session: " } else { "Session: " };
        Some(crate::actions::Entry {
            id: format!("session.{}", session.id),
            label: format!("{kind}{}", names.join(", ")),
            shortcut: Some(match panes
                .iter()
                .find_map(|pane| pane.pwd.as_deref().map(place))
                .filter(|place| !place.is_empty())
            {
                Some(place) => format!("{count} / {place}"),
                None => count,
            }),
            action: AppAction::AttachSession(session.id.clone()),
        })
    };
    listing.sessions.iter().filter_map(row).collect()
}

/// The directory in what a shell reported, which is a URL such as
/// `kitty-shell-cwd://host/C:/src`: the path alone, and a drive without the
/// slash before it.
fn place(pwd: &str) -> String {
    let path = match pwd.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |at| &rest[at..]),
        None => pwd,
    };
    let drive = path.strip_prefix('/').filter(|rest| rest.as_bytes().get(1) == Some(&b':'));
    drive.unwrap_or(path).to_owned()
}

/// How a session being taken is laid out: as its window left it, cut down
/// to the shells that are there to take. Shells its layout does not place,
/// which is all of them when it cannot be read, are tiled after the rest.
/// `None` when the session cannot be taken.
fn arrangement(session: &SessionInfo) -> Option<Saved> {
    let panes: Vec<u32> = takeable(session).iter().map(|pane| pane.pane).collect();
    if panes.is_empty() {
        return None;
    }
    let read = serde_json::from_str::<Saved>(&session.layout).ok();
    let mut saved = read.filter(|saved| saved.version == VERSION).unwrap_or_default();
    saved.version = VERSION;
    saved.keep_only(|pane| panes.contains(&pane));
    let placed = saved.members();
    saved.panes.extend(panes.iter().filter(|pane| !placed.contains(pane)));
    Some(saved)
}

impl TerminalView {
    /// Show a waiting session as a new workspace of this window, and go to
    /// it. In a window that had no terminal it is the only workspace.
    pub(super) fn attach_session(&mut self, id: &str, gl: &glow::Context) -> Result<(), String> {
        // The sessions as they are now, which the rows chosen from followed.
        let listing = crate::session::sessions().ok_or("That session is no longer waiting.")?;
        let session = listing.sessions.iter().find(|session| session.id == id);
        let saved = session.and_then(arrangement).ok_or("That session is no longer waiting.")?;
        let index = self.free_index();
        let workspace_id = self.push_workspace(index, Some(saved.layout)).id;
        // Daemon pane to this window's tiled and floating panes.
        let mut tiled: HashMap<u32, u64> = HashMap::new();
        let mut floating: HashMap<u32, u64> = HashMap::new();
        for number in &saved.panes {
            let Some(pane) = self.adopt(*number, &listing, gl) else { continue };
            let id = self.next_pane;
            self.next_pane += 1;
            self.tiled.push((id, pane));
            tiled.insert(*number, id);
            self.workspace_by_id(workspace_id).panes.push(id);
        }
        if tiled.is_empty() {
            self.workspaces.retain(|workspace| workspace.id != workspace_id);
            return Err("None of the session's shells could be shown.".into());
        }
        for float in &saved.floats {
            let Some(pane) = self.adopt(float.pane, &listing, gl) else { continue };
            let id = self.floating.insert(float.name.clone(), FloatingTerminal { pane });
            floating.insert(float.pane, id);
            self.workspace_by_id(workspace_id).floats.add(id);
        }
        let visible = saved.visible_float.and_then(|pane| floating.get(&pane)).copied();
        let focused = saved.focused.and_then(|pane| tiled.get(&pane)).copied();
        let weights = saved
            .weights
            .iter()
            .map(|(layout, weights)| {
                let weights = weights
                    .iter()
                    .filter_map(|(pane, weight)| Some((*tiled.get(pane)?, *weight)))
                    .collect();
                (*layout, weights)
            })
            .collect();
        let workspace = self.workspace_by_id(workspace_id);
        workspace.layout = saved.layout;
        if let Some(ratio) = saved.main_ratio.filter(|ratio| ratio.is_finite()) {
            workspace.main_ratio = ratio.clamp(0.1, 0.9);
        }
        workspace.pane_weights = weights;
        workspace.appearance =
            saved.appearance.clone().filter(|appearance| appearance.validate().is_ok());
        match visible {
            Some(id) => workspace.floats.show(id),
            None => workspace.floats.hide(),
        }
        if let Some(id) = focused.or(workspace.panes.first().copied()) {
            workspace.focus(id);
        }
        if !id.is_empty() {
            let session = workspace::Session { id: id.to_owned(), sent: String::new() };
            workspace.session = Some(session);
        }
        self.select_workspace(workspace_id)?;
        // A window that had no terminal had a workspace with nothing in it.
        self.prune_workspaces();
        workspace::renumber(&mut self.workspaces);
        self.context.request_repaint();
        Ok(())
    }

    /// A pane on a shell the daemon is running. `None`, with the error
    /// shown, when the terminal could not be made.
    fn adopt(
        &mut self,
        number: u32,
        listing: &crate::session::Listing,
        gl: &glow::Context,
    ) -> Option<super::TerminalPane> {
        let Some(adopt) = listing.adopt(number) else {
            self.window_error(format!("Shell {number} is no longer waiting."));
            return None;
        };
        let options = super::pane::SpawnOptions {
            start: crate::session::Start::Adopt(Box::new(adopt)),
            ..Default::default()
        };
        let pane = self.create_pane(gl, &options);
        match pane {
            Ok(pane) => Some(pane),
            Err(error) => {
                self.window_error(format!("A running shell could not be shown: {error}"));
                None
            }
        }
    }

    fn workspace_by_id(&mut self, id: u64) -> &mut workspace::Workspace {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .expect("the workspace was just made or found")
    }

    /// A workspace by the daemon's pane numbers. Panes with their own
    /// shells, and ones the daemon has not numbered yet, are left out.
    /// `None` when that leaves nothing.
    fn describe(&self, workspace: &workspace::Workspace) -> Option<Saved> {
        let number = |id: &u64| self.tiled_ref(*id).and_then(|pane| pane.session_pane());
        let panes: Vec<u32> = workspace.panes.iter().filter_map(number).collect();
        if panes.is_empty() {
            return None;
        }
        let float = |id: &u64| {
            let entry = self.floating.get(*id)?;
            Some((entry.value.pane.session_pane()?, entry.name.clone()))
        };
        let weights = workspace
            .pane_weights
            .iter()
            .map(|(layout, weights)| {
                let weights = weights
                    .iter()
                    .filter_map(|(id, weight)| Some((number(id)?, *weight)))
                    .collect();
                (*layout, weights)
            })
            .collect();
        Some(Saved {
            version: VERSION,
            layout: workspace.layout,
            main_ratio: Some(workspace.main_ratio),
            focused: workspace.focused.as_ref().and_then(number),
            weights,
            appearance: workspace.appearance.clone(),
            floats: workspace
                .floats
                .ids()
                .iter()
                .filter_map(float)
                .map(|(pane, name)| Float { pane, name })
                .collect(),
            visible_float: workspace.floats.visible().and_then(|id| float(&id)).map(|(pane, _)| pane),
            panes,
        })
    }

    /// Keep the daemon's copy of each workspace up to date. Called every
    /// frame; the daemon hears only of changes. A workspace that goes is
    /// not taken from the daemon: when its shells end, or another
    /// workspace takes them, the daemon forgets it, and one that was
    /// detached is meant to stay.
    pub(super) fn save_sessions(&mut self) {
        for at in 0..self.workspaces.len() {
            // `None` while the daemon has numbered none of its panes.
            let Some(saved) = self.describe(&self.workspaces[at]) else { continue };
            let Ok(text) = serde_json::to_string(&saved) else { continue };
            let session = self.workspaces[at]
                .session
                .get_or_insert_with(|| workspace::Session { id: new_id(), sent: String::new() });
            if session.sent != text {
                crate::session::set_session(session.id.clone(), saved.members(), text.clone());
                session.sent = text;
            }
        }
    }

    /// Take the current workspace out of this window and leave its session
    /// waiting in the daemon. The last workspace takes the window with it.
    pub(super) fn detach_workspace(&mut self) -> Result<(), String> {
        self.save_sessions();
        let id = self.current;
        let workspace = self.workspace();
        let kept = |pane: Option<&super::TerminalPane>| {
            pane.is_some_and(|pane| pane.session_pane().is_some())
        };
        let all_kept = workspace.panes.iter().all(|pane| kept(self.tiled_ref(*pane)))
            && workspace
                .floats
                .ids()
                .iter()
                .all(|float| kept(self.floating.get(*float).map(|entry| &entry.value.pane)));
        // One the daemon has not been told of could not be shown again.
        let told = workspace.session.as_ref().is_some_and(|session| !session.sent.is_empty());
        if !(told && all_kept) {
            return Err("This workspace's shells are not in the session daemon, \
                        so they cannot be left running."
                .into());
        }
        if self.workspaces.len() == 1 {
            // Closing a window detaches what it shows.
            self.close_requested = true;
            return Ok(());
        }
        // Gone from here and left in the daemon.
        let at = self.workspaces.iter().position(|w| w.id == id).expect("current workspace");
        let workspace = self.workspaces.remove(at);
        for pane in &workspace.panes {
            if let Some(at) = self.tiled.iter().position(|(id, _)| id == pane) {
                let (_, mut pane) = self.tiled.remove(at);
                pane.keep_shell();
                self.closed_panes.push(pane);
            }
        }
        for float in workspace.floats.ids() {
            if let Some(mut float) = self.floating.remove(*float) {
                float.pane.keep_shell();
                self.closed_panes.push(float.pane);
            }
        }
        workspace::renumber(&mut self.workspaces);
        let exists = |id: u64| self.workspaces.iter().any(|w| w.id == id);
        self.current =
            if exists(self.previous) { self.previous } else { self.workspaces[0].id };
        self.previous = self.current;
        self.context.request_repaint();
        Ok(())
    }

    /// End every shell in the current workspace, and its session with them.
    pub(super) fn end_workspace(&mut self) {
        let workspace = self.workspace();
        let floats = workspace.floats.ids().to_vec();
        let panes = workspace.panes.clone();
        self.close_floats(floats);
        for pane in panes {
            let _ = self.close_tiled(pane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nterm_session::protocol::{SessionInfo, Size};

    fn pane(number: u32, attached: u32, title: Option<&str>) -> PaneInfo {
        PaneInfo {
            pane: number,
            pid: 1,
            creation_time: 1,
            command: r"C:\Windows\system32\cmd.exe".into(),
            size: Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 },
            exited: None,
            attached,
            title: title.map(str::to_owned),
            pwd: None,
        }
    }

    fn layout(panes: &[u32], floats: &[u32]) -> String {
        let saved = Saved {
            version: VERSION,
            panes: panes.to_vec(),
            floats: floats.iter().map(|pane| Float { pane: *pane, name: None }).collect(),
            ..Saved::default()
        };
        serde_json::to_string(&saved).unwrap()
    }

    fn session(id: &str, layout: String, panes: &[PaneInfo]) -> SessionInfo {
        SessionInfo { id: id.into(), panes: panes.to_vec(), layout }
    }

    fn labels(sessions: Vec<SessionInfo>) -> Vec<String> {
        let rows = session_rows(&crate::session::Listing::of(sessions));
        rows.into_iter().map(|row| row.label).collect()
    }

    #[test]
    fn a_reported_directory_is_shown_as_a_path() {
        assert_eq!(place("kitty-shell-cwd://localhost/C:/src/nterm"), "C:/src/nterm");
        assert_eq!(place("file://host/home/me"), "/home/me");
        assert_eq!(place("C:/src"), "C:/src");
        assert_eq!(place("file://host"), "");
    }

    #[test]
    fn waiting_sessions_are_offered_and_shown_ones_are_not() {
        let sessions = vec![
            session("a", layout(&[1, 2], &[]), &[pane(1, 0, Some("build")), pane(2, 0, None)]),
            // Another window is showing it.
            session("b", layout(&[3], &[]), &[pane(3, 1, None)]),
            // A window shows one of its shells: it is taken whole or not.
            session("c", layout(&[6, 7], &[]), &[pane(6, 1, None), pane(7, 0, None)]),
            // In no session: the one that waits is offered.
            session("", String::new(), &[pane(5, 0, Some("stray")), pane(8, 1, None)]),
        ];
        let rows = session_rows(&crate::session::Listing::of(sessions));
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["session.a", "session."]);
        assert_eq!(rows[0].label, "Session: build, cmd.exe");
        assert_eq!(rows[0].shortcut.as_deref(), Some("2 panes"));
        assert_eq!(rows[1].label, "Shells in no session: stray");
    }

    #[test]
    fn a_session_is_listed_whatever_its_layout_says() {
        // Listing reads no layout: one this build cannot read is a session
        // all the same.
        let other = format!(r#"{{"version":{},"panes":[4]}}"#, VERSION + 1);
        assert_eq!(labels(vec![session("a", other, &[pane(4, 0, None)])]), ["Session: cmd.exe"]);
        assert_eq!(labels(vec![session("a", "nonsense".into(), &[pane(4, 0, None)])]).len(), 1);
    }

    #[test]
    fn a_session_is_taken_as_it_was_laid_out_with_every_shell_it_has() {
        let panes = [pane(1, 0, None), pane(2, 0, None), pane(3, 0, None)];
        // Its float is still a float; a shell the layout leaves out is tiled.
        let saved = arrangement(&session("a", layout(&[2], &[1]), &panes)).unwrap();
        assert_eq!(saved.panes, [2, 3]);
        assert_eq!(saved.floats.len(), 1);
        // Shells that have ended are gone from it; a float left alone is tiled.
        let saved = arrangement(&session("a", layout(&[9], &[1]), &panes[..1])).unwrap();
        assert_eq!(saved.panes, [1]);
        assert!(saved.floats.is_empty());
        // A newer description is read; another version is not, and its
        // shells are tiled in the daemon's order.
        let newer = format!(r#"{{"version":{VERSION},"later":true,"panes":[2,1]}}"#);
        assert_eq!(arrangement(&session("a", newer, &panes[..2])).unwrap().panes, [2, 1]);
        let other = format!(r#"{{"version":{},"panes":[2,1]}}"#, VERSION + 1);
        assert_eq!(arrangement(&session("a", other, &panes[..2])).unwrap().panes, [1, 2]);
        // One a window is showing cannot be taken.
        assert!(arrangement(&session("a", layout(&[1], &[]), &[pane(1, 1, None)])).is_none());
    }
}
