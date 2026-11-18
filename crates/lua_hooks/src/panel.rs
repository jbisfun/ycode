//! `gpui.register_panel{...}`: dock panels rendered from Lua, like `gpui.open_window`.
//!
//! ```lua
//! gpui.register_panel{
//!     id = "pull", title = "Pull", icon = "git_branch", position = "left", size = 320,
//!     state = {}, poll_ms = 1000,
//!     render = function(w) return { "col", { "text", "hello" } } end,
//! }
//! ```
//! `ycode::TogglePanel { id }` shows/focuses or hides it. Panels are rebuilt in every
//! workspace when hooks reload.

use crate::ui::{LuaWindow, ViewOpts};
use crate::{LuaHooks, TogglePanel};
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Global,
    IntoElement, Pixels, Render, Window, px,
};
use std::collections::HashMap;
use std::str::FromStr as _;
use ui::IconName;
use workspace::dock::{DockPosition, Panel, PanelEvent};
use workspace::{MultiWorkspace, Workspace};

pub struct LuaPanel {
    id: String,
    tooltip: &'static str,
    icon: Option<IconName>,
    position: DockPosition,
    size: Pixels,
    /// Docks require a distinct activation priority per panel (status bar order);
    /// Lua panels take 1000 + their registration order, above every native panel.
    priority: u32,
    view: Entity<LuaWindow>,
}

/// Lua panels currently added to each workspace.
#[derive(Default)]
struct LuaPanels(HashMap<EntityId, Vec<Entity<LuaPanel>>>);
impl Global for LuaPanels {}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else { return };
        workspace.register_action(|workspace, action: &TogglePanel, window, cx| {
            toggle(workspace, &action.id, window, cx);
        });
        cx.defer_in(window, |workspace, window, cx| {
            sync_workspace(workspace, window, cx)
        });
    })
    .detach();
}

/// Rebuild Lua panels in every workspace (after the runtime changed).
pub fn sync_all(cx: &mut App) {
    cx.defer(|cx| {
        for handle in cx.windows() {
            let Some(mw) = handle.downcast::<MultiWorkspace>() else {
                continue;
            };
            mw.update(cx, |mw, window, cx| {
                let workspaces: Vec<Entity<Workspace>> = mw.workspaces().cloned().collect();
                for ws in workspaces {
                    ws.update(cx, |ws, cx| sync_workspace(ws, window, cx));
                }
            })
            .ok();
        }
    });
}

fn sync_workspace(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let key = cx.entity().entity_id();
    let old = cx
        .default_global::<LuaPanels>()
        .0
        .remove(&key)
        .unwrap_or_default();
    for panel in &old {
        workspace.remove_panel(panel, window, cx);
    }
    let Some(rt) = LuaHooks::runtime(cx) else {
        return;
    };
    let mut added = Vec::new();
    for (index, (id, opts)) in rt.panels().into_iter().enumerate() {
        let view_opts = match ViewOpts::parse(&rt, &opts, "gpui.register_panel") {
            Ok(o) => o,
            Err(e) => {
                log::error!(target: "lua_hooks", "panel {id}: {e}");
                continue;
            }
        };
        let get = |k: &str| opts.get::<Option<String>>(k).ok().flatten();
        let title = get("title").unwrap_or_else(|| id.clone());
        let icon = get("icon").and_then(|i| IconName::from_str(&i).ok());
        let position = match get("position").as_deref() {
            Some("right") => DockPosition::Right,
            Some("bottom") => DockPosition::Bottom,
            _ => DockPosition::Left,
        };
        let size = px(opts
            .get::<Option<f32>>("size")
            .ok()
            .flatten()
            .unwrap_or(320.));
        let rt = rt.clone();
        let panel = cx.new(|cx| {
            let view = cx.new(|cx| LuaWindow::new(rt, view_opts, true, window, cx));
            LuaPanel {
                id,
                // Panel tooltips are &'static; one small leak per (re)load.
                tooltip: Box::leak(title.into_boxed_str()),
                icon,
                position,
                size,
                priority: 1000 + index as u32,
                view,
            }
        });
        workspace.add_panel(panel.clone(), window, cx);
        added.push(panel);
    }
    cx.default_global::<LuaPanels>().0.insert(key, added);
}

fn toggle(workspace: &mut Workspace, id: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    let key = cx.entity().entity_id();
    let panels = cx
        .default_global::<LuaPanels>()
        .0
        .get(&key)
        .cloned()
        .unwrap_or_default();
    let Some(panel) = panels.into_iter().find(|p| p.read(cx).id == id) else {
        log::warn!(target: "lua_hooks", "no Lua panel {id}");
        return;
    };
    let focus = panel.focus_handle(cx);
    let focused = focus.contains_focused(window, cx);
    let docks: Vec<_> = workspace.all_docks().iter().map(|d| (*d).clone()).collect();
    for dock in docks {
        let Some(ix) = dock.read(cx).panel_index_for_id(panel.entity_id()) else {
            continue;
        };
        let close =
            dock.read(cx).is_open() && dock.read(cx).active_panel_index() == Some(ix) && focused;
        dock.update(cx, |dock, cx| {
            if close {
                dock.set_open(false, window, cx);
            } else {
                dock.activate_panel(ix, window, cx);
                dock.set_open(true, window, cx);
            }
        });
        if !close {
            window.focus(&focus, cx);
        }
        break;
    }
}

impl EventEmitter<PanelEvent> for LuaPanel {}

impl Focusable for LuaPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }
}

impl Render for LuaPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.view.clone()
    }
}

impl Panel for LuaPanel {
    fn persistent_name() -> &'static str {
        "Lua Panel"
    }

    fn panel_key() -> &'static str {
        "LuaPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, _: DockPosition) -> bool {
        true
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        self.size
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        self.icon
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some(self.tooltip)
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(TogglePanel {
            id: self.id.clone(),
        })
    }

    fn activation_priority(&self) -> u32 {
        self.priority
    }
}
