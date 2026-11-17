//! gpui for Lua: `gpui.open_window{...}` opens a real gpui window whose content is
//! rendered from Lua tables on every frame.
//!
//! ```lua
//! gpui.open_window{
//!     title = "hello", width = 600, height = 400,
//!     state = { n = 0 },             -- persistent table, available as w.state
//!     focus = "name",                -- input to focus on open
//!     poll_ms = 500,                 -- re-render periodically (optional)
//!     on_key = function(w, ev) ... return true end,   -- ev.key/ctrl/alt/shift/cmd/char; true = consumed
//!     render = function(w)
//!         return { "col", p = 8, gap = 4,
//!             { "text", "clicked " .. w.state.n .. " times", fg = "muted" },
//!             { "input", id = "name", placeholder = "type here" },
//!             { "button", "Click", on_click = function(w) w.state.n = w.state.n + 1 end },
//!         }
//!     end,
//! }
//! ```
//!
//! Elements are `{ tag, children..., props }` with tag `col` (default), `row`, `text`,
//! `button`, `input`. Strings and numbers are text. Props:
//! - layout: `p px py gap w h` (pixels, or `"full"` for w/h), `grow`, `align`
//!   (`center`/`end`), `justify` (`center`/`end`/`between`)
//! - look: `bg fg border` (theme names: `background surface panel editor element text muted
//!   accent border error warning success`, or `#rrggbb[aa]`), `rounded size mono bold`
//! - behaviour: `id`, `scroll = true` (vertical scroll; needs/uses `id`), `on_click = fn(w)`
//! - inputs (`id` required): `multiline`, `placeholder`; read and write them with
//!   `w.text(id)` and `w.set_text(id, s)`
//!
//! Inside callbacks, `w` also has `focus(id)`, `scroll_bottom(id)`, `refresh()`,
//! `close()` and `state`.

use crate::runtime::{Host, Runtime};
use anyhow::Result;
use editor::Editor;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, Context, Div, Entity, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement, IntoElement, KeystrokeEvent, ParentElement, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowKind, WindowOptions, div, px,
    rgba,
};
use mlua::{Function, Scope, Table, Value};
use platform_title_bar::PlatformTitleBar;
use release_channel::ReleaseChannel;
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::rc::Rc;
use std::time::Duration;
use theme::ActiveTheme;
use workspace::client_side_decorations;

fn rt_err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

pub struct LuaWindow {
    rt: Rc<Runtime>,
    render_fn: Function,
    on_key: Option<Function>,
    state: Table,
    editors: HashMap<String, Entity<Editor>>,
    scrolls: HashMap<String, ScrollHandle>,
    initial_focus: Option<String>,
    auto_id: usize,
    last_error: Option<String>,
    title_bar: Option<Entity<PlatformTitleBar>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _poll: Option<Task<()>>,
}

/// Open a window described by `opts` (see module docs).
pub fn open_window(cx: &mut App, rt: Rc<Runtime>, opts: Table) -> mlua::Result<AnyWindowHandle> {
    let title: String = opts
        .get::<Option<String>>("title")?
        .unwrap_or_else(|| "ycode".into());
    let width: f32 = opts.get::<Option<f32>>("width")?.unwrap_or(720.);
    let height: f32 = opts.get::<Option<f32>>("height")?.unwrap_or(480.);
    let render_fn: Function = opts
        .get::<Option<Function>>("render")?
        .ok_or_else(|| rt_err("gpui.open_window: `render` function is required"))?;
    let on_key: Option<Function> = opts.get("on_key")?;
    let state: Table = match opts.get::<Option<Table>>("state")? {
        Some(t) => t,
        None => rt.lua().create_table()?,
    };
    let initial_focus: Option<String> = opts.get("focus")?;
    let poll_ms: Option<u64> = opts.get("poll_ms")?;

    let size = gpui::size(px(width), px(height));
    let display_id = cx.active_display().map(|d| d.id());
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
        }),
        focus: true,
        show: true,
        is_movable: true,
        kind: WindowKind::Normal,
        window_background: cx.theme().window_background_appearance(),
        app_id: Some(ReleaseChannel::global(cx).app_id().to_owned()),
        window_decorations: Some(WindowDecorations::Client),
        window_bounds: Some(WindowBounds::centered(display_id, size, cx)),
        display_id,
        window_min_size: Some(gpui::size(px(240.), px(160.))),
        ..Default::default()
    };
    let handle = cx
        .open_window(options, |window, cx| {
            cx.new(|cx| {
                let wh = window.window_handle();
                let weak: gpui::WeakEntity<LuaWindow> = cx.entity().downgrade();
                let key_sub = cx.intercept_keystrokes(
                    move |ev: &KeystrokeEvent, window: &mut Window, cx: &mut App| {
                        if window.window_handle() != wh {
                            return;
                        }
                        let Some(this) = weak.upgrade() else {
                            return;
                        };
                        if this.update(cx, |this: &mut LuaWindow, cx| this.key(ev, window, cx)) {
                            cx.stop_propagation();
                        }
                    },
                );
                let poll = poll_ms.map(|ms| {
                    cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor()
                                .timer(Duration::from_millis(ms.max(16)))
                                .await;
                            if this.update(cx, |_, cx| cx.notify()).is_err() {
                                break;
                            }
                        }
                    })
                });
                let title_bar = (!cfg!(target_os = "macos"))
                    .then(|| cx.new(|cx| PlatformTitleBar::new("lua-window-title-bar", cx)));
                LuaWindow {
                    rt,
                    render_fn,
                    on_key,
                    state,
                    editors: HashMap::default(),
                    scrolls: HashMap::default(),
                    initial_focus,
                    auto_id: 0,
                    last_error: None,
                    title_bar,
                    focus_handle: cx.focus_handle(),
                    _subscriptions: vec![key_sub],
                    _poll: poll,
                }
            })
        })
        .map_err(mlua::Error::external)?;
    Ok(handle.into())
}

/// Host for Lua running inside a window callback.
pub struct UiHost<'a, 'b> {
    window: &'a mut Window,
    cx: &'a mut Context<'b, LuaWindow>,
    editors: &'a mut HashMap<String, Entity<Editor>>,
    scrolls: &'a mut HashMap<String, ScrollHandle>,
}

impl Host for UiHost<'_, '_> {
    fn call_orig(&mut self, _args: Option<Json>) -> Result<Option<Json>> {
        Ok(None)
    }

    fn dispatch(&mut self, name: &str, args: Option<Json>) -> Result<()> {
        crate::dispatch_deferred(self.cx, name, args)
    }

    fn action_names(&self) -> Vec<String> {
        crate::action_names(self.cx)
    }

    fn open_window(&mut self, opts: Table) -> Result<()> {
        crate::open_window_deferred(self.cx, opts);
        Ok(())
    }

    fn set_settings(&mut self, settings: Json) -> Result<()> {
        crate::set_plugin_settings_deferred(self.cx, settings);
        Ok(())
    }

    fn kv_get(&mut self, key: &str) -> Result<Option<String>> {
        crate::kv_get(self.cx, key)
    }

    fn kv_set(&mut self, key: &str, value: Option<String>) -> Result<()> {
        crate::kv_set(self.cx, key, value)
    }

    fn palette_hide(&mut self, namespace: &str) -> Result<()> {
        crate::palette_hide(self.cx, namespace)
    }
}

/// The `w` table handed to window callbacks.
fn make_w<'scope, 'env, 'a, 'b, H>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &mlua::Lua,
    cell: &'scope RefCell<H>,
    state: Table,
) -> mlua::Result<Table>
where
    H: DerefMut<Target = UiHost<'a, 'b>>,
    'a: 'scope,
    'b: 'a,
{
    fn busy() -> mlua::Error {
        rt_err("window is busy (nested callback)")
    }
    let w = lua.create_table()?;
    w.set("state", state)?;
    w.set(
        "text",
        scope.create_function(move |_, id: String| {
            let mut guard = cell.try_borrow_mut().map_err(|_| busy())?;
            let u: &mut UiHost = &mut guard;
            Ok(u.editors.get(&id).map(|e| e.read(u.cx).text(u.cx)))
        })?,
    )?;
    w.set(
        "set_text",
        scope.create_function(move |_, (id, text): (String, String)| {
            let mut guard = cell.try_borrow_mut().map_err(|_| busy())?;
            let u: &mut UiHost = &mut guard;
            let editor = u
                .editors
                .get(&id)
                .cloned()
                .ok_or_else(|| rt_err(format!("no input with id {id}")))?;
            let window = &mut *u.window;
            editor.update(&mut *u.cx, |e, cx| e.set_text(text, window, cx));
            Ok(())
        })?,
    )?;
    w.set(
        "focus",
        scope.create_function(move |_, id: String| {
            let mut guard = cell.try_borrow_mut().map_err(|_| busy())?;
            let u: &mut UiHost = &mut guard;
            if let Some(editor) = u.editors.get(&id) {
                let handle = editor.focus_handle(u.cx);
                u.window.focus(&handle, u.cx);
            }
            Ok(())
        })?,
    )?;
    w.set(
        "scroll_bottom",
        scope.create_function(move |_, id: String| {
            let mut guard = cell.try_borrow_mut().map_err(|_| busy())?;
            guard.scrolls.entry(id).or_default().scroll_to_bottom();
            Ok(())
        })?,
    )?;
    w.set(
        "refresh",
        scope.create_function(move |_, ()| {
            let mut guard = cell.try_borrow_mut().map_err(|_| busy())?;
            guard.cx.notify();
            Ok(())
        })?,
    )?;
    w.set(
        "close",
        scope.create_function(move |_, ()| {
            let mut guard = cell.try_borrow_mut().map_err(|_| busy())?;
            guard.window.remove_window();
            Ok(())
        })?,
    )?;
    Ok(w)
}

impl LuaWindow {
    /// Call a Lua callback as `f(w, arg)`.
    fn call(
        &mut self,
        f: &Function,
        arg: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<Value> {
        let rt = self.rt.clone();
        let state = self.state.clone();
        let mut ui = UiHost {
            window,
            cx,
            editors: &mut self.editors,
            scrolls: &mut self.scrolls,
        };
        let cell = RefCell::new(&mut ui);
        rt.with_host(&cell, |scope| {
            let w = make_w(scope, rt.lua(), &cell, state)?;
            f.call::<Value>((w, arg))
        })
    }

    /// Run an event callback; errors are shown in the window and logged.
    fn event(
        &mut self,
        f: &Function,
        arg: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Value {
        let result = self.call(f, arg, window, cx);
        cx.notify();
        match result {
            Ok(v) => {
                self.last_error = None;
                v
            }
            Err(e) => {
                log::error!(target: "lua_hooks", "window callback: {e}");
                self.last_error = Some(e.to_string());
                Value::Nil
            }
        }
    }

    fn key(&mut self, ev: &KeystrokeEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(f) = self.on_key.clone() else {
            return false;
        };
        let k = &ev.keystroke;
        let table = (|| -> mlua::Result<Table> {
            let t = self.rt.lua().create_table()?;
            t.set("key", k.key.as_str())?;
            t.set("char", k.key_char.clone())?;
            t.set("ctrl", k.modifiers.control)?;
            t.set("alt", k.modifiers.alt)?;
            t.set("shift", k.modifiers.shift)?;
            t.set("cmd", k.modifiers.platform)?;
            Ok(t)
        })();
        let Ok(table) = table else {
            return false;
        };
        let consumed = self.event(&f, Value::Table(table), window, cx);
        !matches!(consumed, Value::Nil | Value::Boolean(false))
    }

    fn color(name: &str, cx: &App) -> Option<Hsla> {
        let c = cx.theme().colors();
        let s = cx.theme().status();
        Some(match name {
            "background" => c.background,
            "surface" => c.surface_background,
            "panel" => c.panel_background,
            "editor" => c.editor_background,
            "element" => c.element_background,
            "hover" => c.element_hover,
            "text" => c.text,
            "muted" => c.text_muted,
            "accent" => c.text_accent,
            "border" => c.border,
            "error" => s.error,
            "warning" => s.warning,
            "success" => s.success,
            hex => {
                let hex = hex.strip_prefix('#')?;
                let v = u32::from_str_radix(hex, 16).ok()?;
                match hex.len() {
                    6 => rgba((v << 8) | 0xff).into(),
                    8 => rgba(v).into(),
                    _ => return None,
                }
            }
        })
    }

    fn style(mut el: Div, t: &Table, cx: &App) -> mlua::Result<Div> {
        let num = |k: &str| t.get::<Option<f32>>(k);
        let flag =
            |k: &str| -> mlua::Result<bool> { Ok(t.get::<Option<bool>>(k)?.unwrap_or(false)) };
        let color = |k: &str| -> mlua::Result<Option<Hsla>> {
            Ok(match t.get::<Option<String>>(k)? {
                Some(name) => Some(
                    Self::color(&name, cx).ok_or_else(|| rt_err(format!("bad color {name:?}")))?,
                ),
                None => None,
            })
        };
        if let Some(v) = num("p")? {
            el = el.p(px(v));
        }
        if let Some(v) = num("px")? {
            el = el.px(px(v));
        }
        if let Some(v) = num("py")? {
            el = el.py(px(v));
        }
        if let Some(v) = num("gap")? {
            el = el.gap(px(v));
        }
        if let Some(v) = num("rounded")? {
            el = el.rounded(px(v));
        }
        if let Some(v) = num("size")? {
            el = el.text_size(px(v));
        }
        match t.get::<Value>("w")? {
            Value::String(s) if s.to_str()?.as_ref() == "full" => el = el.w_full(),
            Value::Integer(n) => el = el.w(px(n as f32)),
            Value::Number(n) => el = el.w(px(n as f32)),
            _ => {}
        }
        match t.get::<Value>("h")? {
            Value::String(s) if s.to_str()?.as_ref() == "full" => el = el.h_full(),
            Value::Integer(n) => el = el.h(px(n as f32)).flex_none(),
            Value::Number(n) => el = el.h(px(n as f32)).flex_none(),
            _ => {}
        }
        if flag("grow")? {
            el = el.flex_1().min_h_0().min_w_0();
        }
        if let Some(c) = color("bg")? {
            el = el.bg(c);
        }
        if let Some(c) = color("fg")? {
            el = el.text_color(c);
        }
        if let Some(c) = color("border")? {
            el = el.border_1().border_color(c);
        }
        if flag("mono")? {
            el = el.font_family(".ZedMono");
        }
        if flag("bold")? {
            el = el.font_weight(FontWeight::BOLD);
        }
        match t.get::<Option<String>>("align")?.as_deref() {
            Some("center") => el = el.items_center(),
            Some("end") => el = el.items_end(),
            _ => {}
        }
        match t.get::<Option<String>>("justify")?.as_deref() {
            Some("center") => el = el.justify_center(),
            Some("end") => el = el.justify_end(),
            Some("between") => el = el.justify_between(),
            _ => {}
        }
        Ok(el)
    }

    fn build(&mut self, v: Value, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let text = |s: String| div().child(SharedString::from(s)).into_any_element();
        match v {
            Value::Nil => div().into_any_element(),
            Value::String(s) => text(s.to_string_lossy()),
            Value::Integer(i) => text(i.to_string()),
            Value::Number(n) => text(n.to_string()),
            Value::Boolean(b) => text(b.to_string()),
            Value::Table(t) => match self.build_table(t, window, cx) {
                Ok(el) => el,
                Err(e) => self.error_element(e.to_string(), cx),
            },
            other => text(format!("{other:?}")),
        }
    }

    fn error_element(&self, msg: String, cx: &App) -> AnyElement {
        div()
            .p_1()
            .text_color(cx.theme().status().error)
            .font_family(".ZedMono")
            .child(SharedString::from(msg))
            .into_any_element()
    }

    fn build_table(
        &mut self,
        t: Table,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let (tag, start) = match t.raw_get::<Value>(1)? {
            Value::String(s) => (s.to_str()?.to_string(), 2),
            _ => ("col".to_string(), 1),
        };
        let mut el = div();
        el = match tag.as_str() {
            "col" | "div" => el.flex().flex_col(),
            "row" => el.flex().flex_row(),
            "text" => el,
            "button" => {
                let c = cx.theme().colors();
                el.flex()
                    .flex_none()
                    .items_center()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .border_1()
                    .border_color(c.border)
                    .bg(c.element_background)
                    .hover(|s| s.bg(cx.theme().colors().element_hover))
            }
            "input" => el.flex().flex_col().w_full(),
            other => return Err(rt_err(format!("unknown element {other:?}"))),
        };
        el = Self::style(el, &t, cx)?;

        if tag == "input" {
            let id: String = t
                .get::<Option<String>>("id")?
                .ok_or_else(|| rt_err("input needs an `id`"))?;
            let multiline = t.get::<Option<bool>>("multiline")?.unwrap_or(false);
            let placeholder: Option<String> = t.get("placeholder")?;
            let editor = self
                .editors
                .entry(id.clone())
                .or_insert_with(|| {
                    cx.new(|cx| {
                        let mut e = if multiline {
                            Editor::multi_line(window, cx)
                        } else {
                            Editor::single_line(window, cx)
                        };
                        if let Some(p) = &placeholder {
                            e.set_placeholder_text(p, window, cx);
                        }
                        e
                    })
                })
                .clone();
            if self.initial_focus.as_deref() == Some(id.as_str()) {
                self.initial_focus = None;
                window.focus(&editor.focus_handle(cx), cx);
            }
            return Ok(el.child(editor).into_any_element());
        }

        let len = t.raw_len();
        for i in start..=len {
            let child: Value = t.raw_get(i)?;
            let child = self.build(child, window, cx);
            el = el.child(child);
        }

        let id: Option<String> = t.get("id")?;
        let on_click: Option<Function> = t.get("on_click")?;
        let scroll = t.get::<Option<bool>>("scroll")?.unwrap_or(false);
        if id.is_none() && on_click.is_none() && !scroll {
            return Ok(el.into_any_element());
        }
        let id = id.unwrap_or_else(|| {
            self.auto_id += 1;
            format!("lua-auto-{}", self.auto_id)
        });
        let mut el = el.id(SharedString::from(id.clone()));
        if scroll {
            let handle = self.scrolls.entry(id).or_default().clone();
            el = el.overflow_y_scroll().track_scroll(&handle);
        }
        if let Some(f) = on_click {
            el = el
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.event(&f, Value::Nil, window, cx);
                }));
        }
        Ok(el.into_any_element())
    }
}

impl Focusable for LuaWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LuaWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.auto_id = 0;
        let f = self.render_fn.clone();
        let body = match self.call(&f, Value::Nil, window, cx) {
            Ok(tree) => self.build(tree, window, cx),
            Err(e) => self.error_element(e.to_string(), cx),
        };
        let error = self
            .last_error
            .clone()
            .map(|e| self.error_element(format!("callback error: {e}"), cx));
        let colors = cx.theme().colors();
        client_side_decorations(
            div()
                .flex()
                .flex_col()
                .size_full()
                .bg(colors.editor_background)
                .text_color(colors.text)
                .track_focus(&self.focus_handle)
                .children(self.title_bar.clone())
                .child(div().flex().flex_col().flex_1().min_h_0().child(body))
                .children(error),
            window,
            cx,
        )
    }
}
