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
//! `button`, `icon_button`, `icon`, `checkbox`, `toggle_group`, `divider`,
//! `panel_header`, `list_item`, `list_header`, `disclosure`, `input`. Strings
//! and numbers are text. Props:
//! - `button` (native `ui::Button`, `LabelSize::Small`): label is the first string
//!   child (or `label`), `style = "subtle"` (default) | `"filled"` | `"transparent"`,
//!   `icon`, `tooltip`, `disabled`, `on_click = fn(w)`
//! - `icon_button` (native `ui::IconButton`, `IconSize::Small`): `icon` (required,
//!   snake_case/kebab-case `IconName`, e.g. `"settings"`), `tooltip`, `disabled`,
//!   `on_click = fn(w)`
//! - `icon` (native `ui::Icon`): `name` (required, same `IconName` parsing as
//!   `icon_button`), `size` (`"indicator"`/`"xsmall"`/`"small"`/`"default"`/`"large"`),
//!   `color` (named theme color or `#rrggbb[aa]`, same as `text`'s `color`)
//! - `checkbox` (native `ui::Checkbox`): `checked = true | false | "mixed"`
//!   (`"mixed"` is `ToggleState::Indeterminate`), `label`, `disabled`, `on_click = fn(w)`
//! - `toggle_group` (native `ui::ToggleButtonGroup`, 1-6 options): `options`
//!   (list of strings), `selected` (one of `options`), `on_select = fn(w, option)`
//! - `divider` (native `ui::Divider`): `vertical` (default horizontal)
//! - `panel_header` (height `Tab::container_height`, bottom border): `title`
//!   (Default-size `Label`), `context` (optional Small muted `Label`),
//!   children render right-aligned as actions
//! - layout: `p px py gap w h` (pixels, or `"full"` for w/h), `grow`, `align`
//!   (`start`/`center`/`end`; `row` defaults to `center`, use `align = "start"` to
//!   opt out), `justify` (`center`/`end`/`between`)
//! - look: `bg fg border` (theme names: `background surface panel editor element text muted
//!   accent border error warning success`, or `#rrggbb[aa]`), `rounded mono bold`,
//!   `size` (`"default"`/`"small"`/`"xsmall"`/`"large"`, follows `ui_font_size`; a raw
//!   number is accepted for back-compat but is a fixed px size and logs a deprecation
//!   warning)
//! - behaviour: `id`, `scroll = true` (vertical scroll; needs/uses `id`), `on_click = fn(w)`
//! - `text` renders a native `ui::Label` (never the window's root text size), so bare
//!   strings elsewhere in a tree also render at Label's default (14px) size. `text`-only
//!   props: `size` (`"default"`/`"small"`/`"xsmall"`/`"large"`), `color`/`fg` (named or
//!   `#rrggbb[aa]`), `mono` (buffer font instead of the UI font), `truncate` (ellipsis).
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
use ui::WithScrollbar as _;
use std::ops::DerefMut;
use std::rc::Rc;
use std::time::Duration;
use theme::ActiveTheme;
use ui::{Color, Label, LabelCommon, LabelSize};
use ui::{Disclosure, ListHeader, ListItem, ListItemSpacing, ListSubHeader, Toggleable};
use ui::{Checkbox, Divider, DividerColor, Icon, IconSize, Tab, ToggleButtonGroup, ToggleButtonSimple, ToggleState};
use workspace::client_side_decorations;

fn rt_err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

/// Parses a Lua icon name (`"settings"` or `"settings-off"`) into a native
/// `ui::IconName`. `IconName`'s `EnumString` derive expects snake_case, so
/// kebab-case is normalized first. Shared by every element that takes an
/// `icon` prop (`button`, `icon_button`, and later `icon`, `panel_header`).
fn parse_icon_name(name: &str) -> mlua::Result<ui::IconName> {
    use std::str::FromStr as _;
    let snake = name.replace('-', "_");
    ui::IconName::from_str(&snake).map_err(|_| rt_err(format!("unknown icon {name:?}")))
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

    fn open_path(&mut self, path: String) -> Result<()> {
        crate::open_path_deferred(self.cx, path);
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

    /// Named `ui::Color` for the `text` element's `color` prop, falling back
    /// to the same theme-name/hex parsing as `color()` for a custom color.
    fn label_color(name: &str, cx: &App) -> Option<Color> {
        Some(match name {
            "default" => Color::Default,
            "muted" => Color::Muted,
            "accent" => Color::Accent,
            "error" => Color::Error,
            "warning" => Color::Warning,
            "success" => Color::Success,
            "hidden" => Color::Hidden,
            "disabled" => Color::Disabled,
            "placeholder" => Color::Placeholder,
            "info" => Color::Info,
            "hint" => Color::Hint,
            _ => Color::Custom(Self::color(name, cx)?),
        })
    }

    /// Named `ui::LabelSize` for the `text` element's `size` prop.
    fn label_size(name: &str) -> Option<LabelSize> {
        Some(match name {
            "default" => LabelSize::Default,
            "small" => LabelSize::Small,
            "xsmall" => LabelSize::XSmall,
            "large" => LabelSize::Large,
            _ => return None,
        })
    }

    /// Named `ui::IconSize` for the `icon` element's `size` prop.
    fn icon_size(name: &str) -> mlua::Result<IconSize> {
        Ok(match name {
            "indicator" => IconSize::Indicator,
            "xsmall" => IconSize::XSmall,
            "small" => IconSize::Small,
            "default" | "medium" => IconSize::Medium,
            "large" | "xlarge" => IconSize::XLarge,
            other => return Err(rt_err(format!("bad icon size {other:?}"))),
        })
    }

    /// Lays `buttons` out as a native `ui::ToggleButtonGroup`. `ToggleButtonGroup`
    /// is generic over a const column count, so the Lua-side variable-length
    /// `options` list is dispatched to a fixed-size array per supported size.
    fn build_toggle_group(
        group_name: SharedString,
        buttons: Vec<ToggleButtonSimple>,
        selected_index: usize,
    ) -> mlua::Result<AnyElement> {
        macro_rules! group {
            ($n:literal) => {{
                let arr: [ToggleButtonSimple; $n] = match buttons.try_into() {
                    Ok(arr) => arr,
                    Err(_) => unreachable!("buttons.len() checked above"),
                };
                ToggleButtonGroup::single_row(group_name, arr)
                    .selected_index(selected_index)
                    .into_any_element()
            }};
        }
        Ok(match buttons.len() {
            1 => group!(1),
            2 => group!(2),
            3 => group!(3),
            4 => group!(4),
            5 => group!(5),
            6 => group!(6),
            n => {
                return Err(rt_err(format!(
                    "toggle_group supports 1-6 options, got {n}"
                )));
            }
        })
    }

    /// `{ "text", "...", size = "small", color = "muted", mono, truncate }`
    /// renders a native `ui::Label` instead of a styled div, so bare text
    /// matches the 14px size of native panels instead of the window's root
    /// text size.
    fn build_text(t: &Table, start: usize, cx: &App) -> mlua::Result<AnyElement> {
        let len = t.raw_len();
        let mut s = String::new();
        for i in start..=len {
            let v: Value = t.raw_get(i)?;
            match v {
                Value::Nil => {}
                Value::String(v) => s.push_str(&v.to_string_lossy()),
                Value::Integer(n) => s.push_str(&n.to_string()),
                Value::Number(n) => s.push_str(&n.to_string()),
                Value::Boolean(b) => s.push_str(&b.to_string()),
                other => s.push_str(&format!("{other:?}")),
            }
        }
        let mut label = Label::new(s);
        if let Some(name) = t.get::<Option<String>>("size")? {
            label = label
                .size(Self::label_size(&name).ok_or_else(|| rt_err(format!("bad size {name:?}")))?);
        }
        // `color` is the documented prop; `fg` (used by other elements) is
        // accepted too so `text` stays consistent with `col`/`row`/`button`.
        let color_name = match t.get::<Option<String>>("color")? {
            Some(c) => Some(c),
            None => t.get::<Option<String>>("fg")?,
        };
        if let Some(name) = color_name {
            label = label.color(
                Self::label_color(&name, cx).ok_or_else(|| rt_err(format!("bad color {name:?}")))?,
            );
        }
        if t.get::<Option<bool>>("mono")?.unwrap_or(false) {
            label = label.buffer_font(cx);
        }
        if t.get::<Option<bool>>("truncate")?.unwrap_or(false) {
            label = label.truncate();
        }
        Ok(label.into_any_element())
    }

    /// `button` (native `ui::Button`) and `icon_button` (native `ui::IconButton`):
    /// bordered-div lookalikes are gone, both use the theme's own button styling.
    fn build_button(
        &mut self,
        tag: &str,
        t: &Table,
        start: usize,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        use ui::{ButtonCommon as _, Clickable as _, Disableable as _};

        let id: String = t.get::<Option<String>>("id")?.unwrap_or_else(|| {
            self.auto_id += 1;
            format!("lua-auto-{}", self.auto_id)
        });
        let tooltip: Option<String> = t.get("tooltip")?;
        let disabled = t.get::<Option<bool>>("disabled")?.unwrap_or(false);
        let on_click: Option<Function> = t.get("on_click")?;
        let entity = cx.entity();
        let click = on_click.map(|f| {
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.event(&f, Value::Nil, window, cx);
                });
            }
        });

        let icon_name: Option<String> = t.get("icon")?;
        let icon = match icon_name {
            Some(name) => Some(parse_icon_name(&name)?),
            None => None,
        };

        if tag == "icon_button" {
            let icon = icon.ok_or_else(|| rt_err("icon_button needs an `icon`"))?;
            let mut btn = ui::IconButton::new(id, icon)
                .icon_size(ui::IconSize::Small)
                .disabled(disabled);
            if let Some(tt) = tooltip {
                btn = btn.tooltip(ui::Tooltip::text(tt));
            }
            if let Some(f) = click {
                btn = btn.on_click(f);
            }
            return Ok(btn.into_any_element());
        }

        let label: String = match t.raw_get::<Value>(start)? {
            Value::String(s) => s.to_string_lossy(),
            _ => t.get::<Option<String>>("label")?.unwrap_or_default(),
        };
        let mut btn = ui::Button::new(id, label)
            .label_size(ui::LabelSize::Small)
            .disabled(disabled);
        if let Some(icon) = icon {
            btn = btn.start_icon(ui::Icon::new(icon));
        }
        if let Some(name) = t.get::<Option<String>>("style")? {
            let style = match name.as_str() {
                "subtle" => ui::ButtonStyle::Subtle,
                "filled" => ui::ButtonStyle::Filled,
                "transparent" => ui::ButtonStyle::Transparent,
                other => return Err(rt_err(format!("bad button style {other:?}"))),
            };
            btn = btn.style(style);
        }
        if let Some(tt) = tooltip {
            btn = btn.tooltip(ui::Tooltip::text(tt));
        }
        if let Some(f) = click {
            btn = btn.on_click(f);
        }
        Ok(btn.into_any_element())
    }

    /// A child slot prop (`start`, `end_`, ...): builds the Lua value if
    /// present, otherwise `None` so the native component leaves the slot out.
    fn build_slot(
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

    /// `list_item{ selected, indent, start, end_, on_click, children... }`,
    /// `list_header{ "label", sub, start, end_, end_hover, on_click }`,
    /// `disclosure{ open, on_click }`, `icon{ name, size?, color? }`,
    /// `checkbox{ checked, label?, on_click }`,
    /// `toggle_group{ options, selected, on_select }`, `divider` and
    /// `panel_header{ title, context?, children... }` render the native
    /// `ui::ListItem`, `ui::ListHeader`/`ui::ListSubHeader`, `ui::Disclosure`,
    /// `ui::Icon`, `ui::Checkbox`, `ui::ToggleButtonGroup`, `ui::Divider` and a
    /// `Tab::container_height` header div (fixed row height, hover/selected
    /// background, pointer cursor and the whole row as the hit target come
    /// from those components, not from a styled div).
    fn build_native(
        &mut self,
        tag: &str,
        t: &Table,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let auto_id = |this: &mut Self, prefix: &str| {
            this.auto_id += 1;
            format!("lua-{prefix}-{}", this.auto_id)
        };
        match tag {
            "list_item" => {
                let id = match t.get::<Option<String>>("id")? {
                    Some(id) => id,
                    None => auto_id(self, "list-item"),
                };
                let selected = t.get::<Option<bool>>("selected")?.unwrap_or(false);
                let indent = t.get::<Option<usize>>("indent")?.unwrap_or(0);
                let start_slot = self.build_slot(t, "start", window, cx)?;
                let end_slot = self.build_slot(t, "end_", window, cx)?;
                let on_click: Option<Function> = t.get("on_click")?;
                let mut item = ListItem::new(SharedString::from(id))
                    .spacing(ListItemSpacing::Dense)
                    .indent_level(indent)
                    .toggle_state(selected)
                    .start_slot::<AnyElement>(start_slot)
                    .end_slot::<AnyElement>(end_slot);
                if let Some(f) = on_click {
                    item = item.on_click(cx.listener(move |this, _, window, cx| {
                        this.event(&f, Value::Nil, window, cx);
                    }));
                }
                let len = t.raw_len();
                for i in start..=len {
                    let child: Value = t.raw_get(i)?;
                    let child = self.build(child, window, cx);
                    item = item.child(child);
                }
                Ok(item.into_any_element())
            }
            "list_header" => {
                let label: String = match t.raw_get::<Value>(start)? {
                    Value::String(s) => s.to_string_lossy(),
                    _ => t.get::<Option<String>>("label")?.unwrap_or_default(),
                };
                let sub = t.get::<Option<bool>>("sub")?.unwrap_or(false);
                let start_slot = self.build_slot(t, "start", window, cx)?;
                let end_slot = self.build_slot(t, "end_", window, cx)?;
                if sub {
                    let mut header = ListSubHeader::new(label);
                    if let Some(end_slot) = end_slot {
                        header = header.end_slot(end_slot);
                    }
                    let _ = start_slot; // ListSubHeader only takes an IconName start slot.
                    Ok(header.into_any_element())
                } else {
                    let end_hover_slot = self.build_slot(t, "end_hover", window, cx)?;
                    let mut header = ListHeader::new(label)
                        .start_slot::<AnyElement>(start_slot)
                        .end_slot::<AnyElement>(end_slot)
                        .end_hover_slot::<AnyElement>(end_hover_slot);
                    if let Some(f) = t.get::<Option<Function>>("on_click")? {
                        header = header.on_toggle(cx.listener(move |this, _, window, cx| {
                            this.event(&f, Value::Nil, window, cx);
                        }));
                    }
                    Ok(header.into_any_element())
                }
            }
            "disclosure" => {
                let id = match t.get::<Option<String>>("id")? {
                    Some(id) => id,
                    None => auto_id(self, "disclosure"),
                };
                let open = t.get::<Option<bool>>("open")?.unwrap_or(false);
                let mut disclosure = Disclosure::new(SharedString::from(id), open);
                if let Some(f) = t.get::<Option<Function>>("on_click")? {
                    let handler: std::sync::Arc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)> =
                        std::sync::Arc::new(cx.listener(move |this, _, window, cx| {
                            this.event(&f, Value::Nil, window, cx);
                        }));
                    disclosure = disclosure.on_toggle_expanded(handler);
                }
                Ok(disclosure.into_any_element())
            }
            "icon" => {
                let name: String = t
                    .get::<Option<String>>("name")?
                    .ok_or_else(|| rt_err("icon needs a `name`"))?;
                let mut icon = Icon::new(parse_icon_name(&name)?);
                if let Some(size) = t.get::<Option<String>>("size")? {
                    icon = icon.size(Self::icon_size(&size)?);
                }
                if let Some(color) = t.get::<Option<String>>("color")? {
                    icon = icon.color(
                        Self::label_color(&color, cx)
                            .ok_or_else(|| rt_err(format!("bad color {color:?}")))?,
                    );
                }
                Ok(icon.into_any_element())
            }
            "checkbox" => {
                let id = match t.get::<Option<String>>("id")? {
                    Some(id) => id,
                    None => auto_id(self, "checkbox"),
                };
                let checked = match t.get::<Value>("checked")? {
                    Value::Boolean(true) => ToggleState::Selected,
                    Value::Boolean(false) | Value::Nil => ToggleState::Unselected,
                    Value::String(s) if s.to_str()?.as_ref() == "mixed" => {
                        ToggleState::Indeterminate
                    }
                    other => {
                        return Err(rt_err(format!(
                            "bad checked {other:?}, expected true|false|\"mixed\""
                        )));
                    }
                };
                let disabled = t.get::<Option<bool>>("disabled")?.unwrap_or(false);
                let mut checkbox = Checkbox::new(SharedString::from(id), checked).disabled(disabled);
                if let Some(label) = t.get::<Option<String>>("label")? {
                    checkbox = checkbox.label(label);
                }
                if let Some(f) = t.get::<Option<Function>>("on_click")? {
                    checkbox = checkbox.on_click(cx.listener(move |this, _state, window, cx| {
                        this.event(&f, Value::Nil, window, cx);
                    }));
                }
                Ok(checkbox.into_any_element())
            }
            "toggle_group" => {
                let options: Vec<String> = match t.get::<Value>("options")? {
                    Value::Table(opts) => {
                        let len = opts.raw_len();
                        let mut v = Vec::with_capacity(len);
                        for i in 1..=len {
                            v.push(opts.raw_get::<String>(i)?);
                        }
                        v
                    }
                    _ => return Err(rt_err("toggle_group needs an `options` list")),
                };
                if options.is_empty() {
                    return Err(rt_err("toggle_group needs at least one option"));
                }
                let selected: Option<String> = t.get("selected")?;
                let selected_index = selected
                    .as_deref()
                    .and_then(|s| options.iter().position(|o| o == s))
                    .unwrap_or(0);
                let group_name = match t.get::<Option<String>>("id")? {
                    Some(id) => id,
                    None => auto_id(self, "toggle-group"),
                };
                let on_select: Option<Function> = t.get("on_select")?;
                let entity = cx.entity();
                let rt = self.rt.clone();
                let buttons: Vec<ToggleButtonSimple> = options
                    .into_iter()
                    .map(|opt| {
                        let f = on_select.clone();
                        let entity = entity.clone();
                        let rt = rt.clone();
                        ToggleButtonSimple::new(opt.clone(), move |_, window, cx| {
                            let Some(f) = f.clone() else { return };
                            let Ok(value) = rt.lua().create_string(&opt) else {
                                return;
                            };
                            entity.update(cx, |this, cx| {
                                this.event(&f, Value::String(value), window, cx);
                            });
                        })
                    })
                    .collect();
                Self::build_toggle_group(SharedString::from(group_name), buttons, selected_index)
            }
            "divider" => {
                let divider = if t.get::<Option<bool>>("vertical")?.unwrap_or(false) {
                    Divider::vertical()
                } else {
                    Divider::horizontal()
                };
                Ok(divider.color(DividerColor::BorderVariant).into_any_element())
            }
            "panel_header" => {
                let title: String = t.get::<Option<String>>("title")?.unwrap_or_default();
                let context: Option<String> = t.get("context")?;
                let mut header = ui::h_flex()
                    .h(Tab::container_height(cx))
                    .w_full()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        ui::h_flex()
                            .items_center()
                            .gap_2()
                            .child(Label::new(title).size(LabelSize::Default))
                            .children(context.map(|c| {
                                Label::new(c).size(LabelSize::Small).color(Color::Muted)
                            })),
                    );
                let len = t.raw_len();
                if start <= len {
                    let mut actions = ui::h_flex().items_center().gap_1();
                    for i in start..=len {
                        let child: Value = t.raw_get(i)?;
                        let child = self.build(child, window, cx);
                        actions = actions.child(child);
                    }
                    header = header.child(actions);
                }
                Ok(header.into_any_element())
            }
            other => Err(rt_err(format!("unknown element {other:?}"))),
        }
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
        match t.get::<Value>("size")? {
            Value::String(s) => {
                let rems = match s.to_str()?.as_ref() {
                    "default" => ui::TextSize::Default.rems(cx),
                    "small" => ui::TextSize::Small.rems(cx),
                    "xsmall" => ui::TextSize::XSmall.rems(cx),
                    "large" => ui::TextSize::Large.rems(cx),
                    other => {
                        return Err(rt_err(format!(
                            "bad size {other:?}, expected default|small|xsmall|large"
                        )));
                    }
                };
                el = el.text_size(rems);
            }
            Value::Integer(n) => {
                log::warn!(
                    target: "lua_hooks",
                    "size = {n} (px) is deprecated and ignores ui_font_size; use size = \"default\"|\"small\"|\"xsmall\"|\"large\""
                );
                el = el.text_size(px(n as f32));
            }
            Value::Number(n) => {
                log::warn!(
                    target: "lua_hooks",
                    "size = {n} (px) is deprecated and ignores ui_font_size; use size = \"default\"|\"small\"|\"xsmall\"|\"large\""
                );
                el = el.text_size(px(n as f32));
            }
            _ => {}
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
            Some("start") => el = el.items_start(),
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
        // Bare strings/numbers in a tree render at native Label default size
        // (14px), never the window's root text size.
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
        if tag == "text" {
            return Self::build_text(&t, start, cx);
        }
        if matches!(
            tag.as_str(),
            "list_item"
                | "list_header"
                | "disclosure"
                | "icon"
                | "checkbox"
                | "toggle_group"
                | "divider"
                | "panel_header"
        ) {
            return self.build_native(&tag, &t, start, window, cx);
        }
        if matches!(tag.as_str(), "button" | "icon_button") {
            return self.build_button(&tag, &t, start, cx);
        }
        let mut el = div();
        el = match tag.as_str() {
            "col" | "div" => el.flex().flex_col(),
            "row" => el.flex().flex_row().items_center(),
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
            el = el
                .overflow_y_scroll()
                .track_scroll(&handle)
                .vertical_scrollbar_for(&handle, window, cx);
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
