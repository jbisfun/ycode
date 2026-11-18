//! gpui for Lua: `gpui.open_window{...}` / `gpui.register_panel{...}` render a
//! tree of Lua tables on every frame, built from Zed's own `ui` components so Lua
//! panels look like the native ones without any styling in the plugin.
//!
//! ```lua
//! gpui.register_panel{ id = "demo", title = "Demo", icon = "box", position = "right",
//!     render = function(w)
//!         return { "panel",
//!             header = { title = "Demo", context = "3 items",
//!                        actions = { { "icon_button", icon = "rotate_cw", tooltip = "Refresh",
//!                                      on_click = function(w) end } } },
//!             footer = w.state.message,
//!             { "section", "Items", action = { "icon_button", icon = "plus", tooltip = "Add" },
//!                 { "list_item", "first", detail = "a detail", selected = true,
//!                   start = { "icon", name = "file" }, on_click = function(w) end },
//!             },
//!             { "toolbar", { "button", "Build", on_click = build }, { "button", "Test" } },
//!         }
//!     end }
//! ```
//!
//! An element is `{ tag, children..., props }`; bare strings and numbers are text.
//! Props are typed: an unknown or mistyped prop is an error drawn in place of the
//! element. Sizes and spacing are names, never pixels (pixel numbers still work
//! but log a deprecation once).
//!
//! Structure (these carry Zed's spacing, so plugins don't pick any):
//! - `panel{ header, footer, scroll = true, children }`: `header` is a title string or
//!   `{ title, context, actions = {...} }` (tab-bar height, bottom border); the body
//!   fills the rest and scrolls with a scrollbar unless `scroll = false`; `footer` is
//!   a string (small, muted, truncated; `color` applies to it) or an element.
//! - `section{ "Title", action = el, empty = "nothing here", children }`: a list
//!   sub-header (small, muted) followed by its rows.
//! - `toolbar{ children }`: a wrapping row of buttons with toolbar padding.
//! - `split{ a, b, ratio, vertical }`: two panes with a divider. Horizontal splits give
//!   the first pane a sidebar width unless `ratio` (0..1) is set; vertical ones split
//!   evenly by default.
//! - `empty{ "message", action = el }`: centered muted text for empty/loading/error states.
//! - `panel_header{ title, context, actions | children }`, `divider{ vertical }`.
//! - `col` (default tag), `row` (children vertically centered): plain flex containers
//!   with `gap p px py` (`none xs sm md lg xl`), `w h` (`"full"`, `"sidebar"`),
//!   `grow`, `align` (`start center end`), `justify` (`center end between`),
//!   `bg border` (theme color names), `rounded`, `scroll`, `id`, `on_click`.
//!
//! Content:
//! - `text{ "...", size, color, mono, bold, truncate }` (`ui::Label`; `size` is
//!   `default small xsmall large`; `color` is a theme color such as `muted accent
//!   error warning success`; `fg` is an alias).
//! - `list_item{ "label" | children, detail, selected, indent, start, end_, end_hover,
//!   on_click }` (`ui::ListItem`: hover/selected background, full-row hit target;
//!   `detail` is small muted text after the label; `end_hover` replaces `end_` on hover).
//! - `list_header{ "label", sub, start, end_, end_hover, on_click }`.
//! - `button{ "label", style = subtle|filled|transparent, icon, tooltip, disabled,
//!   on_click }`, `icon_button{ icon, tooltip, disabled, on_click }`.
//! - `icon{ name, size, color }`, `checkbox{ checked = true|false|"mixed", label, size,
//!   disabled, on_click }`, `toggle_group{ options, selected, on_select = fn(w, opt) }`,
//!   `disclosure{ open, on_click }`.
//!
//! `gpui.open_window` also takes `kind = "floating"` (stays above the focused window,
//! not tiled) or `"dialog"` (modal), besides `title width height state focus poll_ms
//! on_key render`.
//! - `input{ id, placeholder, multiline }`: read/write with `w.text(id)`/`w.set_text`.
//!
//! Inside callbacks, `w` has `state`, `text(id)`, `set_text(id, s)`, `focus(id)`,
//! `scroll_bottom(id)`, `refresh()` and `close()`.

mod basic;
mod layout;
mod lists;
mod props;

use crate::runtime::{Host, Runtime};
use anyhow::Result;
use editor::Editor;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, ClickEvent, Context, Entity, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeystrokeEvent, ParentElement, Render, ScrollHandle, SharedString,
    Styled, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions, div, px,
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
use props::{Props, rt_err};
use ui::{Label, LabelCommon, LabelSize};
use workspace::client_side_decorations;

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
    /// Rendered inside a dock panel instead of its own window.
    in_panel: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _poll: Option<Task<()>>,
}

/// The Lua side of a view: `render`, `on_key`, `state`, `focus`, `poll_ms` from `opts`.
pub(crate) struct ViewOpts {
    render_fn: Function,
    on_key: Option<Function>,
    state: Table,
    initial_focus: Option<String>,
    poll_ms: Option<u64>,
}

impl ViewOpts {
    pub(crate) fn parse(rt: &Runtime, opts: &Table, what: &str) -> mlua::Result<Self> {
        Ok(Self {
            render_fn: opts
                .get::<Option<Function>>("render")?
                .ok_or_else(|| rt_err(format!("{what}: `render` function is required")))?,
            on_key: opts.get("on_key")?,
            state: match opts.get::<Option<Table>>("state")? {
                Some(t) => t,
                None => rt.lua().create_table()?,
            },
            initial_focus: opts.get("focus")?,
            poll_ms: opts.get("poll_ms")?,
        })
    }
}

impl LuaWindow {
    /// A Lua-rendered view, for a window of its own or (`in_panel`) a dock panel.
    pub(crate) fn new(
        rt: Rc<Runtime>,
        o: ViewOpts,
        in_panel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let wh = window.window_handle();
        let weak: gpui::WeakEntity<LuaWindow> = cx.entity().downgrade();
        let focus_handle = cx.focus_handle();
        let key_focus = focus_handle.clone();
        let key_sub = cx.intercept_keystrokes(
            move |ev: &KeystrokeEvent, window: &mut Window, cx: &mut App| {
                if window.window_handle() != wh {
                    return;
                }
                // A panel shares the workspace window: only keys typed inside it.
                if in_panel && !key_focus.contains_focused(window, cx) {
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
        let poll = o.poll_ms.map(|ms| {
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
        let title_bar = (!in_panel && !cfg!(target_os = "macos"))
            .then(|| cx.new(|cx| PlatformTitleBar::new("lua-window-title-bar", cx)));
        LuaWindow {
            rt,
            render_fn: o.render_fn,
            on_key: o.on_key,
            state: o.state,
            editors: HashMap::default(),
            scrolls: HashMap::default(),
            initial_focus: o.initial_focus,
            auto_id: 0,
            last_error: None,
            title_bar,
            in_panel,
            focus_handle,
            _subscriptions: vec![key_sub],
            _poll: poll,
        }
    }
}

/// Open a window described by `opts` (see module docs).
pub fn open_window(cx: &mut App, rt: Rc<Runtime>, opts: Table) -> mlua::Result<AnyWindowHandle> {
    let title: String = opts
        .get::<Option<String>>("title")?
        .unwrap_or_else(|| "ycode".into());
    let width: f32 = opts.get::<Option<f32>>("width")?.unwrap_or(720.);
    let height: f32 = opts.get::<Option<f32>>("height")?.unwrap_or(480.);
    let view_opts = ViewOpts::parse(&rt, &opts, "gpui.open_window")?;
    // `kind = "floating"` keeps a small window (a prompt, a note box) above the
    // focused window instead of being tiled like a normal one.
    let kind = match opts.get::<Option<String>>("kind")?.as_deref() {
        None | Some("normal") => WindowKind::Normal,
        Some("floating") => WindowKind::Floating,
        Some("dialog") => WindowKind::Dialog,
        Some(other) => {
            return Err(rt_err(format!(
                "gpui.open_window: bad kind {other:?}, expected normal|floating|dialog"
            )));
        }
    };

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
        kind,
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
            cx.new(|cx| LuaWindow::new(rt, view_opts, false, window, cx))
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
    in_panel: bool,
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

    fn workspace_folders(&self) -> Vec<String> {
        crate::workspace_folders(self.cx)
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

    fn spawn(&mut self, cmd: String, callback: mlua::Function) -> Result<()> {
        crate::spawn_command(self.cx, cmd, callback)
    }

    fn open_path(&mut self, path: String, line: Option<u32>) -> Result<()> {
        crate::open_path_deferred(self.cx, path, line);
        Ok(())
    }

    fn selection(&mut self, callback: mlua::Function) -> Result<()> {
        crate::selection_deferred(self.cx, callback);
        Ok(())
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
            // A panel can't close its workspace window; hide its dock instead.
            if guard.in_panel {
                crate::dispatch_deferred(guard.cx, "workspace::CloseActiveDock", None).ok();
            } else {
                guard.window.remove_window();
            }
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
            in_panel: self.in_panel,
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

    /// Shows an error in place of an element (or the whole view).
    fn error_element(&self, msg: String, cx: &App) -> AnyElement {
        div()
            .p_1()
            .text_color(cx.theme().status().error)
            .font_family(".ZedMono")
            .child(SharedString::from(msg))
            .into_any_element()
    }

    /// A stable-per-frame id for elements that need one and weren't given one.
    fn auto_id(&mut self, prefix: &str) -> String {
        self.auto_id += 1;
        format!("lua-{prefix}-{}", self.auto_id)
    }

    fn id_or_auto(&mut self, p: &Props, prefix: &str) -> String {
        match &p.id {
            Some(id) => id.clone(),
            None => self.auto_id(prefix),
        }
    }

    /// A click handler that runs the Lua callback `f(w)`.
    fn click(
        &self,
        f: Function,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let entity = cx.entity();
        move |_, window, cx| {
            entity.update(cx, |this, cx| {
                this.event(&f, Value::Nil, window, cx);
            });
        }
    }

    fn callback(t: &Table, key: &str) -> mlua::Result<Option<Function>> {
        t.get(key)
    }

    /// Concatenated positional text (`{ "text", "a", n, "b" }`).
    fn text_of(t: &Table, start: usize) -> mlua::Result<Option<String>> {
        let len = t.raw_len();
        if start > len {
            return Ok(None);
        }
        let mut s = String::new();
        for i in start..=len {
            match t.raw_get::<Value>(i)? {
                Value::Nil => {}
                Value::String(v) => s.push_str(&v.to_string_lossy()),
                Value::Integer(n) => s.push_str(&n.to_string()),
                Value::Number(n) => s.push_str(&n.to_string()),
                Value::Boolean(b) => s.push_str(&b.to_string()),
                other => return Err(rt_err(format!("expected text, got {}", other.type_name()))),
            }
        }
        Ok(Some(s))
    }

    /// Positional children from `start`.
    fn children(
        &mut self,
        t: &Table,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<Vec<AnyElement>> {
        let len = t.raw_len();
        let mut out = Vec::with_capacity(len.saturating_sub(start) + 1);
        for i in start..=len {
            let child: Value = t.raw_get(i)?;
            out.push(self.build(child, window, cx));
        }
        Ok(out)
    }

    /// An element-valued prop (`start`, `end_`, `footer`, ...).
    fn slot(
        &mut self,
        t: &Table,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<Option<AnyElement>> {
        Ok(match t.get::<Value>(key)? {
            Value::Nil => None,
            v => Some(self.build(v, window, cx)),
        })
    }

    /// A list-of-elements prop (`actions = { el, el }`); a single element is
    /// accepted too.
    fn slot_list(
        &mut self,
        t: &Table,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<Vec<AnyElement>> {
        match t.get::<Value>(key)? {
            Value::Nil => Ok(Vec::new()),
            Value::Table(list) if !matches!(list.raw_get::<Value>(1)?, Value::String(_)) => {
                self.children(&list, 1, window, cx)
            }
            v => Ok(vec![self.build(v, window, cx)]),
        }
    }

    fn build(&mut self, v: Value, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Bare strings/numbers render at native Label size, never the window's
        // root text size.
        let text = |s: String| Label::new(s).size(LabelSize::Default).into_any_element();
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
        let p = Props::parse(self.rt.lua(), &t)
            .map_err(|e| rt_err(format!("{tag}: {e}")))?;
        let r = match tag.as_str() {
            "text" => self.build_text(&t, &p, start, cx),
            "button" | "icon_button" => self.build_button(&tag, &t, &p, start, cx),
            "icon" => Self::build_icon(&p, cx),
            "checkbox" => self.build_checkbox(&t, &p, cx),
            "toggle_group" => self.build_toggle_group(&t, &p, cx),
            "disclosure" => self.build_disclosure(&t, &p, cx),
            "divider" => Ok(Self::build_divider(&p)),
            "list_item" => self.build_list_item(&t, &p, start, window, cx),
            "list_header" => self.build_list_header(&t, &p, start, window, cx),
            "section" => self.build_section(&t, &p, start, window, cx),
            "panel" => self.build_panel(&t, &p, start, window, cx),
            "panel_header" => self.build_panel_header(&t, &p, start, window, cx),
            "toolbar" => self.build_toolbar(&t, &p, start, window, cx),
            "split" => self.build_split(&t, &p, start, window, cx),
            "empty" => self.build_empty(&t, &p, start, window, cx),
            "input" => self.build_input(&p, window, cx),
            "col" | "div" | "row" => self.build_flex(&tag, &t, &p, start, window, cx),
            other => Err(rt_err(format!("unknown element {other:?}"))),
        };
        r.map_err(|e| match e {
            mlua::Error::RuntimeError(m) if !m.starts_with(&format!("{tag}:")) => {
                rt_err(format!("{tag}: {m}"))
            }
            e => e,
        })
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
        if self.in_panel {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .bg(colors.panel_background)
                .text_color(colors.text)
                .track_focus(&self.focus_handle)
                .child(div().flex().flex_col().flex_1().min_h_0().child(body))
                .children(error)
                .into_any_element();
        }
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
        .into_any_element()
    }
}
