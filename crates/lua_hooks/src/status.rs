//! `ycode.status_button{...}`: buttons in every workspace's status bar, defined in Lua.

use crate::{LuaHooks, dispatch_deferred};
use gpui::{App, Context, IntoElement, Render, SharedString, Window};
use std::str::FromStr as _;
use ui::{Button, IconButton, IconName, IconSize, LabelSize, Tooltip, h_flex, prelude::*};
use workspace::{HideStatusItem, StatusItemView, Workspace, item::ItemHandle};

/// Renders the current runtime's buttons; re-read on every render, so a hook reload
/// (followed by a window refresh) shows the new set.
pub struct LuaStatusButtons;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else { return };
        let item = cx.new(|_| LuaStatusButtons);
        workspace
            .status_bar()
            .update(cx, |bar, cx| bar.add_right_item(item, window, cx));
    })
    .detach();
}

impl Render for LuaStatusButtons {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let buttons = LuaHooks::runtime(cx)
            .map(|rt| rt.status_buttons())
            .unwrap_or_default();
        h_flex().gap_1().children(buttons.into_iter().map(|b| {
            let id = SharedString::from(format!("lua-status-{}", b.id));
            let tooltip: SharedString = b
                .tooltip
                .clone()
                .or_else(|| b.label.clone())
                .unwrap_or_else(|| b.id.clone())
                .into();
            let (action, args) = (b.action.clone(), b.args.clone());
            let on_click = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                click(cx, &action, args.clone());
            };
            match b.icon.as_deref().and_then(|i| IconName::from_str(i).ok()) {
                Some(icon) => IconButton::new(id, icon)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text(tooltip))
                    .on_click(on_click)
                    .into_any_element(),
                None => Button::new(id, b.label.clone().unwrap_or(b.id.clone()))
                    .label_size(LabelSize::Small)
                    .tooltip(Tooltip::text(tooltip))
                    .on_click(on_click)
                    .into_any_element(),
            }
        }))
    }
}

/// `%new` actions go through `ycode::Run`; anything else is dispatched by name.
fn click(cx: &mut App, action: &str, args: Option<serde_json::Value>) {
    let is_new = LuaHooks::runtime(cx).is_some_and(|rt| rt.is_new(action));
    let result = if is_new {
        dispatch_deferred(
            cx,
            "ycode::Run",
            Some(serde_json::json!({ "name": action, "args": args })),
        )
    } else {
        dispatch_deferred(cx, action, args)
    };
    if let Err(e) = result {
        log::error!(target: "lua_hooks", "status button {action}: {e:#}");
    }
}

impl StatusItemView for LuaStatusButtons {
    fn set_active_pane_item(
        &mut self,
        _: Option<&dyn ItemHandle>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    /// Each button belongs to a Lua extension; disable the extension to hide it.
    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        None
    }
}
