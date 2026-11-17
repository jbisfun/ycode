//! Lua hooks for ycode, in the spirit of Logos/MobileSubstrate tweaks.
//!
//! Every action dispatched in the app (keyboard, menus, command palette, `dispatch_action`)
//! goes through [`gpui::App::intercept_actions`]. Hook files can wrap any action by name,
//! call through to the original with `%orig`, replace its arguments, or swallow it, and can
//! define new actions with `%new` that are bound to keys through `ycode::Run`.
//!
//! Files are loaded from `<config dir>/hooks/` in name order: `*.x.lua` files go through the
//! Logos-style preprocessor in [`preprocess`]; plain `*.lua` files can use the same runtime
//! via `ycode.hook(name, fn)`, `ycode.new_action(name, fn)` and `ycode.ctor(fn)`.
//! Run `ycode: reload hooks` to reload them.

pub mod preprocess;
pub mod runtime;
pub mod ui;

pub use runtime::{Host, Runtime};

use anyhow::Result;
use command_palette_hooks::{CommandPaletteFilter, GlobalCommandPaletteSource};
use gpui::{Action, AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Global, actions};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value as Json;
use settings::SettingsStore;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// Runs a Lua action defined with `%new` (or `ycode.new_action`) in a hook file.
///
/// Keymap example: `"ctrl-alt-s": ["ycode::Run", { "name": "editor::Shout", "args": { "text": "hi" } }]`
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = ycode)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// Full action name, e.g. `editor::Shout`.
    pub name: String,
    /// Value passed to the Lua function's first parameter.
    #[serde(default)]
    pub args: Option<Json>,
}

actions!(
    ycode,
    [
        /// Reloads Lua hook files from the hooks directory.
        ReloadHooks,
    ]
);

/// `<config dir>/hooks`, e.g. `~/.config/zed/hooks`.
pub fn hooks_dir() -> PathBuf {
    paths::config_dir().join("hooks")
}

#[derive(Default, Clone)]
struct LuaHooks {
    runtime: Rc<RefCell<Option<Rc<Runtime>>>>,
    /// Windows opened from Lua; closed when the runtime is replaced.
    windows: Rc<RefCell<Vec<AnyWindowHandle>>>,
}

impl Global for LuaHooks {}

impl LuaHooks {
    fn runtime(cx: &App) -> Option<Rc<Runtime>> {
        cx.try_global::<LuaHooks>()?.runtime.borrow().clone()
    }
}

/// Build an action by name and dispatch it once the current dispatch has finished.
/// Dispatches to the active window, or, when that is a Lua window or nothing is focused,
/// to an editor (workspace) window, where workspace/agent/editor actions are handled.
pub(crate) fn dispatch_deferred(cx: &mut App, name: &str, args: Option<Json>) -> Result<()> {
    let action = cx.build_action(name, args)?;
    cx.defer(move |cx| {
        let is_lua = |w: &AnyWindowHandle| w.downcast::<ui::LuaWindow>().is_some();
        let target = cx
            .active_window()
            .filter(|w| !is_lua(w))
            .or_else(|| cx.windows().into_iter().find(|w| !is_lua(w)));
        match target {
            Some(window) => {
                window
                    .update(cx, |_, window, cx| {
                        window.dispatch_action(action.boxed_clone(), cx)
                    })
                    .ok();
            }
            None => cx.dispatch_action(action.as_ref()),
        }
    });
    Ok(())
}

pub(crate) fn action_names(cx: &App) -> Vec<String> {
    cx.all_action_names()
        .iter()
        .map(|n| n.to_string())
        .collect()
}

/// Apply the merged `ycode.settings` layer after the current update.
pub(crate) fn set_plugin_settings_deferred(cx: &mut App, settings: Json) {
    cx.defer(move |cx| {
        if !cx.has_global::<SettingsStore>() {
            return;
        }
        let content = settings.to_string();
        cx.update_global(|store: &mut SettingsStore, cx| {
            if let Err(e) = store.set_plugin_settings(&content, cx) {
                log::error!(target: "lua_hooks", "ycode.settings: {e:#}");
            }
        });
    });
}

/// `ycode.kv_get`: read from the app's key-value store (where dismissed banners live).
pub(crate) fn kv_get(cx: &App, key: &str) -> Result<Option<String>> {
    db::kvp::KeyValueStore::global(cx).read_kvp(key)
}

/// `ycode.kv_set`: write synchronously, so code reading the key right after sees it.
pub(crate) fn kv_set(cx: &App, key: &str, value: Option<String>) -> Result<()> {
    let store = db::kvp::KeyValueStore::global(cx);
    let key = key.to_string();
    gpui::block_on(async move {
        match value {
            Some(v) => store.write_kvp(key, v).await,
            None => store.delete_kvp(key).await,
        }
    })
}

/// Namespaces hidden from the command palette by Lua; shown again on reload.
#[derive(Default)]
struct HiddenNamespaces(Vec<&'static str>);
impl Global for HiddenNamespaces {}

pub(crate) fn palette_hide(cx: &mut App, namespace: &str) -> Result<()> {
    let hidden = cx.default_global::<HiddenNamespaces>();
    let ns: &'static str = match hidden.0.iter().find(|n| **n == namespace) {
        Some(n) => n,
        // Leaked once per distinct namespace; the filter only takes &'static str.
        None => {
            let n: &'static str = Box::leak(namespace.to_string().into_boxed_str());
            hidden.0.push(n);
            n
        }
    };
    CommandPaletteFilter::update_global(cx, |filter, _| filter.hide_namespace(ns));
    Ok(())
}

fn palette_reset(cx: &mut App) {
    let hidden: Vec<&'static str> = cx.default_global::<HiddenNamespaces>().0.clone();
    CommandPaletteFilter::update_global(cx, |filter, _| {
        for ns in hidden {
            filter.show_namespace(ns);
        }
    });
}

/// `module::Name` -> `module: name`, like the command palette shows registered actions.
fn humanize(name: &str) -> String {
    command_palette::humanize_action_name(name)
}

/// `ycode.spawn`: run a shell command on the background executor, then call the Lua
/// callback on the main thread, unless the runtime was reloaded in the meantime.
pub(crate) fn spawn_command(cx: &mut App, cmd: String, callback: mlua::Function) -> Result<()> {
    let Some(rt) = LuaHooks::runtime(cx) else {
        anyhow::bail!("no Lua runtime");
    };
    let weak = Rc::downgrade(&rt);
    let job = cx.background_spawn(async move {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(std::process::Stdio::null())
            .output()
    });
    cx.spawn(async move |cx| {
        let output = job.await;
        cx.update(|cx| {
            let (Some(rt), Some(current)) = (weak.upgrade(), LuaHooks::runtime(cx)) else {
                return;
            };
            if !Rc::ptr_eq(&rt, &current) {
                return;
            }
            let (ok, text, code) = match output {
                Ok(o) => {
                    let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&o.stderr));
                    (o.status.success(), text, o.status.code().unwrap_or(-1))
                }
                Err(e) => (false, e.to_string(), -1),
            };
            let mut host = ActionHost {
                cx,
                action: None,
                orig: None,
            };
            let cell = RefCell::new(&mut host);
            if let Err(e) = rt.with_host(&cell, |_| callback.call::<()>((ok, text, code))) {
                log::error!(target: "lua_hooks", "ycode.spawn callback: {e}");
            }
            drop(cell);
            let errors = rt.run_ctors(&mut host);
            for e in errors {
                log::error!(target: "lua_hooks", "{e}");
            }
        });
    })
    .detach();
    Ok(())
}

/// Open a Lua window after the current update (Lua may be running inside another window).
pub(crate) fn open_window_deferred(cx: &mut App, opts: mlua::Table) {
    cx.defer(move |cx| {
        let Some(rt) = LuaHooks::runtime(cx) else {
            return;
        };
        match ui::open_window(cx, rt, opts) {
            Ok(handle) => {
                if let Some(hooks) = cx.try_global::<LuaHooks>() {
                    hooks.windows.borrow_mut().push(handle);
                }
            }
            Err(e) => log::error!(target: "lua_hooks", "gpui.open_window: {e}"),
        }
    });
}

/// [`Host`] for an action dispatch (or a ctor / `%new` action when `orig` is `None`).
struct ActionHost<'a> {
    cx: &'a mut App,
    action: Option<&'a dyn Action>,
    orig: Option<&'a mut dyn FnMut(&dyn Action, &mut App)>,
}

impl Host for ActionHost<'_> {
    fn call_orig(&mut self, args: Option<Json>) -> Result<Option<Json>> {
        let (Some(action), Some(orig)) = (self.action, self.orig.as_mut()) else {
            return Ok(None);
        };
        match args {
            None => orig(action, self.cx),
            Some(args) => {
                let replacement = self.cx.build_action(action.name(), Some(args))?;
                orig(replacement.as_ref(), self.cx);
            }
        }
        Ok(None)
    }

    fn dispatch(&mut self, name: &str, args: Option<Json>) -> Result<()> {
        dispatch_deferred(self.cx, name, args)
    }

    fn action_names(&self) -> Vec<String> {
        action_names(self.cx)
    }

    fn open_window(&mut self, opts: mlua::Table) -> Result<()> {
        open_window_deferred(self.cx, opts);
        Ok(())
    }

    fn set_settings(&mut self, settings: Json) -> Result<()> {
        set_plugin_settings_deferred(self.cx, settings);
        Ok(())
    }

    fn kv_get(&mut self, key: &str) -> Result<Option<String>> {
        kv_get(self.cx, key)
    }

    fn kv_set(&mut self, key: &str, value: Option<String>) -> Result<()> {
        kv_set(self.cx, key, value)
    }

    fn palette_hide(&mut self, namespace: &str) -> Result<()> {
        palette_hide(self.cx, namespace)
    }

    fn spawn(&mut self, cmd: String, callback: mlua::Function) -> Result<()> {
        spawn_command(self.cx, cmd, callback)
    }
}

/// [`Host`] for a Rust-side hook point (see [`call_point`]).
struct PointHost<'a> {
    cx: &'a mut App,
    args: Json,
    orig: &'a mut dyn FnMut(Json, &mut App) -> Json,
}

impl Host for PointHost<'_> {
    fn call_orig(&mut self, args: Option<Json>) -> Result<Option<Json>> {
        let args = args.unwrap_or_else(|| self.args.clone());
        Ok(Some((self.orig)(args, self.cx)))
    }

    fn dispatch(&mut self, name: &str, args: Option<Json>) -> Result<()> {
        dispatch_deferred(self.cx, name, args)
    }

    fn action_names(&self) -> Vec<String> {
        action_names(self.cx)
    }

    fn open_window(&mut self, opts: mlua::Table) -> Result<()> {
        open_window_deferred(self.cx, opts);
        Ok(())
    }

    fn set_settings(&mut self, settings: Json) -> Result<()> {
        set_plugin_settings_deferred(self.cx, settings);
        Ok(())
    }

    fn kv_get(&mut self, key: &str) -> Result<Option<String>> {
        kv_get(self.cx, key)
    }

    fn kv_set(&mut self, key: &str, value: Option<String>) -> Result<()> {
        kv_set(self.cx, key, value)
    }

    fn palette_hide(&mut self, namespace: &str) -> Result<()> {
        palette_hide(self.cx, namespace)
    }

    fn spawn(&mut self, cmd: String, callback: mlua::Function) -> Result<()> {
        spawn_command(self.cx, cmd, callback)
    }
}

/// Expose a named hook point to `%hookf name(args)`. Without a matching hook this is just
/// `orig(args, cx)`. A hook's return value (if not nil) replaces the result.
pub fn call_point(
    cx: &mut App,
    name: &str,
    args: Json,
    mut orig: impl FnMut(Json, &mut App) -> Json,
) -> Json {
    let Some(rt) = LuaHooks::runtime(cx).filter(|rt| rt.wants(name)) else {
        return orig(args, cx);
    };
    let mut result = None;
    let mut orig = |a: Json, cx: &mut App| {
        let r = orig(a, cx);
        result = Some(r.clone());
        r
    };
    let mut host = PointHost {
        cx,
        args: args.clone(),
        orig: &mut orig,
    };
    match rt.invoke(name, name, Some(args), &mut host) {
        Ok(Some(v)) => v,
        Ok(None) => result.unwrap_or(Json::Null),
        Err(e) => {
            log::error!(target: "lua_hooks", "{name}: {e:#}");
            result.unwrap_or(Json::Null)
        }
    }
}

pub fn init(cx: &mut App) {
    let hooks = LuaHooks::default();
    cx.set_global(hooks.clone());
    // Tests install their own runtime instead of the user's hooks directory.
    #[cfg(not(test))]
    load(cx);

    cx.intercept_actions(move |action, orig, cx| {
        let Some(rt) = hooks.runtime.borrow().clone() else {
            orig(action, cx);
            return;
        };

        if let Some(run) = action.as_any().downcast_ref::<Run>() {
            if !rt.is_new(&run.name) && !rt.wants(&run.name) {
                log::warn!(target: "lua_hooks", "ycode::Run: no Lua action named {}", run.name);
                return;
            }
            let debug = format!("{action:?}");
            let mut host = ActionHost {
                cx,
                action: None,
                orig: None,
            };
            if let Err(e) = rt.invoke(&run.name, &debug, run.args.clone(), &mut host) {
                log::error!(target: "lua_hooks", "{}: {e:#}", run.name);
            }
            return;
        }

        let name = action.name();
        if !rt.wants(name) {
            orig(action, cx);
            return;
        }
        let debug = format!("{action:?}");
        let mut host = ActionHost {
            cx,
            action: Some(action),
            orig: Some(orig),
        };
        // Errors are logged by the runtime, which also falls back to the original action.
        rt.invoke(name, &debug, None, &mut host).ok();
    });

    cx.on_action(|_: &ReloadHooks, cx| load(cx));
    // `%new` actions show up in the command palette like registered ones.
    GlobalCommandPaletteSource::set(cx, |cx| {
        let Some(rt) = LuaHooks::runtime(cx) else {
            return Vec::new();
        };
        rt.new_actions()
            .into_iter()
            .map(|name| {
                let action: Box<dyn Action> = Box::new(Run {
                    name: name.clone(),
                    args: None,
                });
                (humanize(&name), action)
            })
            .collect()
    });

    #[cfg(all(unix, not(test)))]
    executor::start(cx);
}

/// Evaluate a REPL request on the main thread. `.reload` reloads hook files.
#[cfg_attr(any(test, not(unix)), allow(dead_code))]
fn eval_request(cx: &mut App, code: &str) -> Json {
    if code.trim() == ".reload" {
        load(cx);
    }
    let Some(rt) = LuaHooks::runtime(cx) else {
        return serde_json::json!({ "ok": false, "error": "no Lua runtime", "log": [] });
    };
    let result = if code.trim().is_empty() || code.trim() == ".reload" {
        Ok(String::new())
    } else {
        rt.eval(
            code,
            &mut ActionHost {
                cx,
                action: None,
                orig: None,
            },
        )
    };
    let log = rt.take_log();
    match result {
        Ok(r) => serde_json::json!({ "ok": true, "result": r, "log": log }),
        Err(e) => serde_json::json!({ "ok": false, "error": format!("{e:#}"), "log": log }),
    }
}

/// Live Lua executor: a Unix socket speaking one JSON object per line.
/// Request `{"code": "..."}`, response `{"ok", "result" | "error", "log"}`, where `log` is
/// everything `ycode.log` printed since the previous request (including from hooks).
#[cfg(all(unix, not(test)))]
mod executor {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    pub fn socket_path() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("ycode-lua.sock")
    }

    type Request = (String, smol::channel::Sender<Json>);

    pub fn start(cx: &mut App) {
        let path = socket_path();
        std::fs::remove_file(&path).ok();
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                log::error!(target: "lua_hooks", "executor: binding {}: {e}", path.display());
                return;
            }
        };
        log::info!(target: "lua_hooks", "executor listening on {}", path.display());
        let (tx, rx) = smol::channel::unbounded::<Request>();
        std::thread::Builder::new()
            .name("lua-executor".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let tx = tx.clone();
                    std::thread::spawn(move || serve(stream, tx));
                }
            })
            .ok();
        cx.spawn(async move |cx| {
            while let Ok((code, reply)) = rx.recv().await {
                let response = cx.update(|cx| eval_request(cx, &code));
                reply.send(response).await.ok();
            }
        })
        .detach();
    }

    fn serve(stream: UnixStream, tx: smol::channel::Sender<Request>) {
        let Ok(mut writer) = stream.try_clone() else {
            return;
        };
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let code = serde_json::from_str::<Json>(&line)
                .ok()
                .and_then(|j| j.get("code")?.as_str().map(String::from))
                .unwrap_or(line);
            let (reply_tx, reply_rx) = smol::channel::bounded(1);
            if tx.send_blocking((code, reply_tx)).is_err() {
                return;
            }
            let Ok(response) = reply_rx.recv_blocking() else {
                return;
            };
            if writeln!(writer, "{response}").is_err() {
                return;
            }
        }
    }
}

/// (Re)load every hook file and run its `%ctor`s, replacing the previous runtime.
fn load(cx: &mut App) {
    let dir = hooks_dir();
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            log::error!(target: "lua_hooks", "creating Lua runtime: {e:#}");
            return;
        }
    };
    let mut module_dirs = vec![dir.join("lib"), dir.clone()];
    for ext in Runtime::extension_dirs(&dir) {
        module_dirs.push(ext.join("lib"));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        // luarocks --local
        module_dirs.push(home.join(".luarocks/share/lua/5.4"));
        module_dirs.push(home.join(".luarocks/lib/lua/5.4"));
    }
    // Distro-packaged modules (Debian/Ubuntu lua-* packages).
    module_dirs.push(PathBuf::from("/usr/lib/x86_64-linux-gnu/lua/5.4"));
    module_dirs.push(PathBuf::from("/usr/share/lua/5.4"));
    if let Err(e) = rt.add_module_dirs(&module_dirs) {
        log::error!(target: "lua_hooks", "setting package paths: {e:#}");
    }
    let errors = rt.load_dir(&dir);
    let errors = install(cx, rt, errors);
    log::info!(target: "lua_hooks", "hooks dir {} ({} errors)", dir.display(), errors.len());
}

/// Run pending `%ctor`s and make `rt` the active runtime. Returns all errors.
pub fn install(cx: &mut App, rt: Runtime, mut errors: Vec<String>) -> Vec<String> {
    // Palette filters and the plugin settings layer belong to the runtime being replaced.
    palette_reset(cx);
    if cx.has_global::<SettingsStore>() {
        cx.update_global(|store: &mut SettingsStore, cx| store.set_plugin_settings("", cx))
            .ok();
    }
    let rt = Rc::new(rt);
    if let Some(hooks) = cx.try_global::<LuaHooks>().cloned() {
        // Windows run callbacks in the old Lua state; close them with it.
        for handle in hooks.windows.take() {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
        // Installed before ctors run, so they can use APIs that need it (ycode.spawn).
        *hooks.runtime.borrow_mut() = Some(rt.clone());
    }
    errors.extend(rt.run_ctors(&mut ActionHost {
        cx,
        action: None,
        orig: None,
    }));
    for e in &errors {
        log::error!(target: "lua_hooks", "{e}");
    }
    log::info!(target: "lua_hooks", "{} active hooks", rt.active_hooks().len());
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::cell::Cell;

    actions!(
        test_ns,
        [
            /// Counted.
            Ping,
            /// Swallowed by the hook.
            Blocked,
        ]
    );

    #[gpui::test]
    fn intercepts_real_dispatch(cx: &mut TestAppContext) {
        let pings = Rc::new(Cell::new(0));
        let blocked = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let p = pings.clone();
            cx.on_action(move |_: &Ping, _| p.set(p.get() + 1));
            let b = blocked.clone();
            cx.on_action(move |_: &Blocked, _| b.set(b.get() + 1));

            init(cx);
            let rt = Runtime::new().unwrap();
            rt.load_str(
                "t.x.lua",
                r#"
%hook test_ns
- Ping
    %orig
    %orig
end
- Blocked
    %log("nope")
end
%new - Twice
    ycode.dispatch("test_ns::Ping")
end
%end
"#,
                true,
            )
            .unwrap();
            assert!(install(cx, rt, Vec::new()).is_empty());

            cx.dispatch_action(&Ping);
            cx.dispatch_action(&Blocked);
            cx.dispatch_action(&Run {
                name: "test_ns::Twice".into(),
                args: None,
            });
        });
        cx.run_until_parked();
        // Ping: 2 from the direct dispatch (double %orig) + 2 from Twice's deferred dispatch.
        assert_eq!(pings.get(), 4);
        assert_eq!(blocked.get(), 0);
    }
}
