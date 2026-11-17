//! mlua runtime: loads hook files, keeps the hook index, and runs `%orig` chains.
//! Independent of gpui; the app side is reached through [`Host`].

use crate::preprocess;
use anyhow::{Context as _, Result};
use mlua::{Function, Lua, LuaSerdeExt, MultiValue, Scope, Table, Value, Variadic};
use serde_json::Value as Json;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

/// What a hook can reach in the host application during one invocation.
pub trait Host {
    /// Run the original implementation, optionally with replacement arguments.
    fn call_orig(&mut self, args: Option<Json>) -> Result<Option<Json>>;
    /// Dispatch an action by name (e.g. `editor::SelectAll`).
    fn dispatch(&mut self, name: &str, args: Option<Json>) -> Result<()>;
    /// Every action name the app knows about.
    fn action_names(&self) -> Vec<String>;
    /// `gpui.open_window{...}` (see `ui.rs`).
    fn open_window(&mut self, _opts: Table) -> Result<()> {
        anyhow::bail!("gpui.open_window is not available here")
    }
    /// Persistent key-value store (the app database). `None` deletes.
    fn kv_get(&mut self, _key: &str) -> Result<Option<String>> {
        anyhow::bail!("ycode.kv is not available here")
    }
    fn kv_set(&mut self, _key: &str, _value: Option<String>) -> Result<()> {
        anyhow::bail!("ycode.kv is not available here")
    }
    /// Run `sh -c cmd` off the main thread; later call `callback(ok, output, code)`.
    fn spawn(&mut self, _cmd: String, _callback: Function) -> Result<()> {
        anyhow::bail!("ycode.spawn is not available here")
    }
    /// Hide every action in `namespace` from the command palette.
    fn palette_hide(&mut self, _namespace: &str) -> Result<()> {
        anyhow::bail!("ycode.palette_hide is not available here")
    }
    /// Replace the plugin settings layer with `settings` (all `ycode.settings` calls merged).
    fn set_settings(&mut self, _settings: Json) -> Result<()> {
        anyhow::bail!("ycode.settings is not available here")
    }
    /// Folders open in workspace windows, the active window's first.
    fn workspace_folders(&self) -> Vec<String> {
        Vec::new()
    }
    /// Open a file in the workspace (editor tab).
    fn open_path(&mut self, _path: String) -> Result<()> {
        anyhow::bail!("ycode.open is not available here")
    }
}

/// Recursively merge `patch` into `base` (objects merge, everything else replaces).
pub fn merge_json(base: &mut Json, patch: Json) {
    match (base, patch) {
        (Json::Object(b), Json::Object(p)) => {
            for (k, v) in p {
                merge_json(b.entry(k).or_insert(Json::Null), v);
            }
        }
        (b, p) => *b = p,
    }
}

const PRELUDE: &str = r#"
__ycode = { hooks = {}, ctors = {} }
function __ycode.def(group, name, is_new, src)
  local h = { group = group, name = name, new = is_new, src = src }
  __ycode.hooks[#__ycode.hooks + 1] = h
  __ycode.touch()
  return h
end
function __ycode.ctor(src)
  local c = { src = src }
  __ycode.ctors[#__ycode.ctors + 1] = c
  return c
end
function ycode.hook(name, fn) __ycode.def(nil, name, false, "ycode.hook(" .. name .. ")").fn = fn end
function ycode.new_action(name, fn) __ycode.def(nil, name, true, "ycode.new_action(" .. name .. ")").fn = fn end
function ycode.ctor(fn) __ycode.ctor("ycode.ctor").fn = fn end
"#;

#[derive(Clone)]
struct Hook {
    name: String,
    src: String,
    is_new: bool,
    func: Function,
}

#[derive(Default)]
struct Shared {
    dirty: Cell<bool>,
    groups: RefCell<HashSet<String>>,
    log: RefCell<Vec<String>>,
    status_buttons: RefCell<Vec<StatusButton>>,
    /// `gpui.register_panel{...}` specs by id, in registration order.
    panels: RefCell<Vec<(String, Table)>>,
}

/// A button in every workspace's status bar, from `ycode.status_button{...}`.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct StatusButton {
    /// Identifies the button; registering the same id again replaces it.
    pub id: String,
    /// Action to dispatch on click (a `%new` action or any registered action).
    pub action: String,
    #[serde(default)]
    pub args: Option<Json>,
    /// Icon name (snake_case `IconName`, e.g. `"settings"`); a label is shown otherwise.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub tooltip: Option<String>,
}

pub struct Runtime {
    lua: Lua,
    shared: Rc<Shared>,
    index: RefCell<Vec<Hook>>,
    /// Everything passed to `ycode.settings`, merged.
    settings: RefCell<Json>,
}

/// `editor::Copy` matches `editor::Copy`, `editor::*` and `*` / `*::*`.
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    if pattern == name || pattern == "*" || pattern == "*::*" {
        return true;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) if prefix.ends_with("::") => name.starts_with(prefix),
        _ => false,
    }
}

/// `lib/` holds `require`-able modules, and dot-directories (.git, ...) are never extensions.
fn is_module_or_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_none_or(|n| n == "lib" || n.starts_with('.'))
}

fn lua_err(e: mlua::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

impl Runtime {
    pub fn new() -> Result<Self> {
        Self::new_lua().map_err(lua_err)
    }

    fn new_lua() -> mlua::Result<Self> {
        // Full standard library and C modules (`require` of .so files). Hook files are
        // trusted user code, like an editor init.lua; safe mode refuses C modules.
        // SAFETY: only `debug`/C-module loading become available; nothing else differs.
        let lua = unsafe { Lua::unsafe_new_with(mlua::StdLib::ALL, mlua::LuaOptions::new()) };
        let shared = Rc::new(Shared::default());
        let ycode = lua.create_table()?;

        let s = shared.clone();
        ycode.set(
            "log",
            lua.create_function(move |lua, args: Variadic<Value>| {
                let tostring: Function = lua.globals().get("tostring")?;
                let parts = args
                    .into_iter()
                    .map(|v| tostring.call::<String>(v))
                    .collect::<mlua::Result<Vec<_>>>()?;
                let line = parts.join(" ");
                log::info!(target: "lua_hooks", "{line}");
                let mut log = s.log.borrow_mut();
                if log.len() >= 1000 {
                    log.drain(..500);
                }
                log.push(line);
                Ok(())
            })?,
        )?;

        // JSON via serde_json (null <-> nil, arrays <-> sequences).
        ycode.set(
            "json_decode",
            lua.create_function(|lua, s: String| {
                let v: Json = serde_json::from_str(&s).map_err(mlua::Error::external)?;
                lua.to_value(&v)
            })?,
        )?;
        ycode.set(
            "json_encode",
            lua.create_function(|lua, v: Value| {
                let v: Json = lua.from_value(v)?;
                serde_json::to_string(&v).map_err(mlua::Error::external)
            })?,
        )?;

        // `print` goes to the same place as ycode.log (hook log / executor), not stdout.
        lua.globals().set("print", ycode.get::<Function>("log")?)?;

        // LuaSocket (socket, socket.http, ltn12, mime, ...) built in.
        mlua_socket::preload(&lua)?;

        let s = shared.clone();
        ycode.set(
            "init",
            lua.create_function(move |_, group: Option<String>| {
                if let Some(g) = group {
                    s.groups.borrow_mut().insert(g);
                    s.dirty.set(true);
                }
                Ok(())
            })?,
        )?;

        let s = shared.clone();
        ycode.set(
            "status_button",
            lua.create_function(move |lua, spec: Value| {
                let button: StatusButton = lua.from_value(spec)?;
                let mut buttons = s.status_buttons.borrow_mut();
                buttons.retain(|b| b.id != button.id);
                buttons.push(button);
                Ok(())
            })?,
        )?;

        let s = shared.clone();
        ycode.set(
            "take_log",
            lua.create_function(move |_, ()| Ok(std::mem::take(&mut *s.log.borrow_mut())))?,
        )?;

        let gpui = lua.create_table()?;
        let s = shared.clone();
        gpui.set(
            "register_panel",
            lua.create_function(move |_, opts: Table| {
                let id: String = opts
                    .get::<Option<String>>("id")?
                    .ok_or_else(|| mlua::Error::runtime("gpui.register_panel: `id` is required"))?;
                let mut panels = s.panels.borrow_mut();
                panels.retain(|(i, _)| *i != id);
                panels.push((id, opts));
                Ok(())
            })?,
        )?;

        for (table, name) in [
            (&ycode, "dispatch"),
            (&ycode, "actions"),
            (&ycode, "eval"),
            (&ycode, "settings"),
            (&ycode, "kv_get"),
            (&ycode, "kv_set"),
            (&ycode, "palette_hide"),
            (&ycode, "spawn"),
            (&gpui, "open_window"),
        ] {
            let msg = format!("{name} is only available inside a hook, %ctor or window callback");
            table.set(
                name,
                lua.create_function(move |_, _: MultiValue| -> mlua::Result<()> {
                    Err(mlua::Error::runtime(msg.clone()))
                })?,
            )?;
        }
        lua.globals().set("ycode", ycode)?;
        lua.globals().set("gpui", gpui)?;
        lua.load(PRELUDE).set_name("=ycode").exec()?;

        let s = shared.clone();
        let internal: Table = lua.globals().get("__ycode")?;
        internal.set(
            "touch",
            lua.create_function(move |_, ()| {
                s.dirty.set(true);
                Ok(())
            })?,
        )?;
        shared.dirty.set(true);
        Ok(Self {
            lua,
            shared,
            index: RefCell::default(),
            settings: RefCell::new(Json::Object(Default::default())),
        })
    }

    /// Prepend directories to `package.path` / `package.cpath` so `require` finds modules
    /// next to hook files (`dir/?.lua`, `dir/?/init.lua`, `dir/?.so`).
    pub fn add_module_dirs(&self, dirs: &[std::path::PathBuf]) -> Result<()> {
        let package: Table = self.lua.globals().get("package").map_err(lua_err)?;
        let mut path: String = package.get("path").map_err(lua_err)?;
        let mut cpath: String = package.get("cpath").map_err(lua_err)?;
        for dir in dirs.iter().rev() {
            let d = dir.display();
            path = format!("{d}/?.lua;{d}/?/init.lua;{path}");
            cpath = format!("{d}/?.so;{cpath}");
        }
        package.set("path", path).map_err(lua_err)?;
        package.set("cpath", cpath).map_err(lua_err)?;
        Ok(())
    }

    /// Load one chunk. `logos` runs the `%hook` preprocessor first.
    pub fn load_str(&self, name: &str, src: &str, logos: bool) -> Result<()> {
        let code = if logos {
            preprocess::preprocess(name, src)?
        } else {
            src.to_string()
        };
        self.lua
            .load(&code)
            .set_name(format!("@{name}"))
            .exec()
            .map_err(lua_err)
    }

    /// Load `*.lua` and `*.x.lua` files from `dir` in name order, then each extension
    /// subdirectory the same way. Returns per-file errors; a broken file does not stop
    /// the others from loading.
    pub fn load_dir(&self, dir: &Path) -> Vec<String> {
        self.load_tree(dir)
    }

    fn load_files(&self, dir: &Path) -> Vec<String> {
        self.load_level(dir, false)
    }

    fn load_tree(&self, dir: &Path) -> Vec<String> {
        self.load_level(dir, true)
    }

    fn load_level(&self, dir: &Path, with_subdirs: bool) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "lua"))
            .collect();
        files.sort();
        let mut errors = Vec::new();
        for path in files {
            let logos = path.to_string_lossy().ends_with(".x.lua");
            let res = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))
                .and_then(|src| self.load_str(&path.to_string_lossy(), &src, logos));
            if let Err(e) = res {
                errors.push(format!("{e:#}"));
            }
        }
        if with_subdirs {
            // Extensions: one level of subdirectories (not `lib/`, which holds modules).
            for sub in Self::extension_dirs(dir) {
                errors.extend(self.load_files(&sub));
            }
        }
        errors
    }

    /// Set a string field on the `ycode` table (e.g. `ycode.hooks_dir`).
    pub fn set_ycode_field(&self, key: &str, value: &str) -> Result<()> {
        let ycode: Table = self.lua.globals().get("ycode").map_err(lua_err)?;
        ycode.set(key, value).map_err(lua_err)
    }

    /// Panels registered with `gpui.register_panel`, in registration order.
    pub fn panels(&self) -> Vec<(String, Table)> {
        self.shared.panels.borrow().clone()
    }

    /// Buttons registered with `ycode.status_button`, in registration order.
    pub fn status_buttons(&self) -> Vec<StatusButton> {
        self.shared.status_buttons.borrow().clone()
    }

    /// Extension directories under `dir` (for adding their `lib/` to package paths).
    pub fn extension_dirs(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut dirs: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && !is_module_or_hidden(p))
            .collect();
        dirs.sort();
        dirs
    }

    /// Run (and consume) pending `%ctor` blocks.
    pub fn run_ctors<D: Host + ?Sized>(&self, host: &mut D) -> Vec<String> {
        let mut errors = Vec::new();
        let ctors: Vec<Table> = (|| -> mlua::Result<Vec<Table>> {
            let internal: Table = self.lua.globals().get("__ycode")?;
            let list: Table = internal.get("ctors")?;
            internal.set("ctors", self.lua.create_table()?)?;
            list.sequence_values().collect()
        })()
        .unwrap_or_default();
        let host = RefCell::new(host);
        for ctor in ctors {
            let src: String = ctor.get("src").unwrap_or_default();
            let Ok(func) = ctor.get::<Function>("fn") else {
                continue;
            };
            let res = self
                .lua
                .scope(|scope| self.with_api(scope, &host, || func.call::<()>(())));
            if let Err(e) = res {
                errors.push(format!("{src}: {e}"));
            }
        }
        errors
    }

    fn refresh(&self) {
        if !self.shared.dirty.replace(false) {
            return;
        }
        let groups = self.shared.groups.borrow();
        let hooks = (|| -> mlua::Result<Vec<Hook>> {
            let internal: Table = self.lua.globals().get("__ycode")?;
            let list: Table = internal.get("hooks")?;
            let mut out = Vec::new();
            for h in list.sequence_values::<Table>() {
                let h = h?;
                let group: Option<String> = h.get("group")?;
                if group.is_some_and(|g| !groups.contains(&g)) {
                    continue;
                }
                let Ok(func) = h.get::<Function>("fn") else {
                    continue;
                };
                out.push(Hook {
                    name: h.get("name")?,
                    src: h.get("src")?,
                    is_new: h.get("new")?,
                    func,
                });
            }
            Ok(out)
        })();
        match hooks {
            Ok(h) => *self.index.borrow_mut() = h,
            Err(e) => log::error!(target: "lua_hooks", "reading hook table: {e}"),
        }
    }

    /// Cheap check used before entering Lua for every action.
    pub fn wants(&self, name: &str) -> bool {
        self.refresh();
        self.index
            .borrow()
            .iter()
            .any(|h| pattern_matches(&h.name, name))
    }

    /// Is `name` an action defined with `%new`?
    pub fn is_new(&self, name: &str) -> bool {
        self.refresh();
        self.index
            .borrow()
            .iter()
            .any(|h| h.is_new && h.name == name)
    }

    /// Names of active `%new` actions (for the command palette).
    pub fn new_actions(&self) -> Vec<String> {
        self.refresh();
        let mut names: Vec<String> = self
            .index
            .borrow()
            .iter()
            .filter(|h| h.is_new)
            .map(|h| h.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn active_hooks(&self) -> Vec<(String, String)> {
        self.refresh();
        self.index
            .borrow()
            .iter()
            .map(|h| (h.name.clone(), h.src.clone()))
            .collect()
    }

    /// Run a REPL chunk (directives allowed). Expressions are returned like the `lua`
    /// REPL does; any `%ctor` it defines runs immediately.
    pub fn eval<D: Host + ?Sized>(&self, code: &str, host: &mut D) -> Result<String> {
        let code = preprocess::preprocess("repl", code)?;
        let func = match self
            .lua
            .load(format!("return {code}"))
            .set_name("=repl")
            .into_function()
        {
            Ok(f) => f,
            Err(_) => self
                .lua
                .load(&code)
                .set_name("=repl")
                .into_function()
                .map_err(lua_err)?,
        };
        let host = RefCell::new(host);
        let out = self
            .lua
            .scope(|scope| {
                self.with_api(scope, &host, || {
                    let tostring: Function = self.lua.globals().get("tostring")?;
                    func.call::<MultiValue>(())?
                        .into_iter()
                        .map(|v| tostring.call::<String>(v))
                        .collect::<mlua::Result<Vec<_>>>()
                })
            })
            .map_err(lua_err)?;
        let errors = self.run_ctors(host.into_inner());
        if !errors.is_empty() {
            anyhow::bail!("{}", errors.join("\n"));
        }
        Ok(out.join("\t"))
    }

    /// Lines passed to `ycode.log` since the last call.
    pub fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.log.borrow_mut())
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Run `f` in a Lua scope with the host-bound API (`ycode.dispatch`, `gpui.open_window`,
    /// ...) pointing at `host`. `f` may create more scoped functions borrowing `host`.
    pub fn with_host<'env, H, D, R>(
        &'env self,
        host: &'env RefCell<H>,
        f: impl for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> mlua::Result<R>,
    ) -> mlua::Result<R>
    where
        H: std::ops::DerefMut<Target = D>,
        D: Host + ?Sized,
    {
        self.lua
            .scope(|scope| self.with_api(scope, host, || f(scope)))
    }

    /// Bind `ycode.dispatch` / `ycode.actions` / `ycode.eval` / `gpui.open_window` to
    /// `host` while `f` runs.
    fn with_api<'scope, 'env, H, D, R>(
        &'scope self,
        scope: &'scope Scope<'scope, 'env>,
        host: &'scope RefCell<H>,
        f: impl FnOnce() -> mlua::Result<R>,
    ) -> mlua::Result<R>
    where
        H: std::ops::DerefMut<Target = D>,
        D: Host + ?Sized,
    {
        let ycode: Table = self.lua.globals().get("ycode")?;
        let gpui: Table = self.lua.globals().get("gpui")?;
        let prev_dispatch: Value = ycode.get("dispatch")?;
        let prev_actions: Value = ycode.get("actions")?;
        let prev_eval: Value = ycode.get("eval")?;
        let prev_settings: Value = ycode.get("settings")?;
        let prev_kv_get: Value = ycode.get("kv_get")?;
        let prev_kv_set: Value = ycode.get("kv_set")?;
        let prev_palette_hide: Value = ycode.get("palette_hide")?;
        let prev_spawn: Value = ycode.get("spawn")?;
        let prev_folders: Value = ycode.get("workspace_folders")?;
        let prev_open_path: Value = ycode.get("open")?;
        ycode.set(
            "open",
            scope.create_function(move |_, path: String| {
                let mut host = host
                    .try_borrow_mut()
                    .map_err(|_| mlua::Error::runtime("ycode.open: host busy"))?;
                host.open_path(path).map_err(mlua::Error::external)
            })?,
        )?;
        ycode.set(
            "workspace_folders",
            scope.create_function(move |_, ()| {
                let host = host
                    .try_borrow()
                    .map_err(|_| mlua::Error::runtime("ycode.workspace_folders: host busy"))?;
                Ok(host.workspace_folders())
            })?,
        )?;
        let busy = |what: &'static str| move |_| mlua::Error::runtime(format!("{what}: host busy"));
        ycode.set(
            "kv_get",
            scope.create_function(move |_, key: String| {
                let mut host = host.try_borrow_mut().map_err(busy("ycode.kv_get"))?;
                host.kv_get(&key).map_err(mlua::Error::external)
            })?,
        )?;
        ycode.set(
            "kv_set",
            scope.create_function(move |_, (key, value): (String, Option<String>)| {
                let mut host = host.try_borrow_mut().map_err(busy("ycode.kv_set"))?;
                host.kv_set(&key, value).map_err(mlua::Error::external)
            })?,
        )?;
        ycode.set(
            "spawn",
            scope.create_function(move |_, (cmd, callback): (String, Function)| {
                let mut host = host.try_borrow_mut().map_err(busy("ycode.spawn"))?;
                host.spawn(cmd, callback).map_err(mlua::Error::external)
            })?,
        )?;
        ycode.set(
            "palette_hide",
            scope.create_function(move |_, namespaces: mlua::Variadic<String>| {
                let mut host = host.try_borrow_mut().map_err(busy("ycode.palette_hide"))?;
                for ns in namespaces {
                    host.palette_hide(&ns).map_err(mlua::Error::external)?;
                }
                Ok(())
            })?,
        )?;
        ycode.set(
            "settings",
            scope.create_function(move |lua, patch: Value| {
                let patch: Json = lua.from_value(patch)?;
                let merged = {
                    let mut settings = self.settings.borrow_mut();
                    merge_json(&mut settings, patch);
                    settings.clone()
                };
                let mut host = host
                    .try_borrow_mut()
                    .map_err(|_| mlua::Error::runtime("ycode.settings: host busy"))?;
                host.set_settings(merged).map_err(mlua::Error::external)
            })?,
        )?;
        let prev_open: Value = gpui.get("open_window")?;
        ycode.set(
            "eval",
            scope.create_function(move |_, code: String| {
                let mut host = host
                    .try_borrow_mut()
                    .map_err(|_| mlua::Error::runtime("ycode.eval: host busy"))?;
                Ok(match self.eval(&code, &mut **host) {
                    Ok(r) => (true, r),
                    Err(e) => (false, format!("{e:#}")),
                })
            })?,
        )?;
        gpui.set(
            "open_window",
            scope.create_function(move |_, opts: Table| {
                let mut host = host
                    .try_borrow_mut()
                    .map_err(|_| mlua::Error::runtime("gpui.open_window: host busy"))?;
                host.open_window(opts).map_err(mlua::Error::external)
            })?,
        )?;
        ycode.set(
            "dispatch",
            scope.create_function(move |lua, (name, args): (String, Option<Value>)| {
                let mut args = args.map(|v| lua.from_value::<Json>(v)).transpose()?;
                let mut name = name;
                if self.is_new(&name) {
                    args = Some(serde_json::json!({ "name": name, "args": args }));
                    name = "ycode::Run".into();
                }
                let mut host = host
                    .try_borrow_mut()
                    .map_err(|_| mlua::Error::runtime("ycode.dispatch: host busy"))?;
                host.dispatch(&name, args).map_err(mlua::Error::external)
            })?,
        )?;
        ycode.set(
            "actions",
            scope.create_function(move |_, ()| {
                let host = host
                    .try_borrow()
                    .map_err(|_| mlua::Error::runtime("ycode.actions: host busy"))?;
                Ok(host.action_names())
            })?,
        )?;
        let result = f();
        ycode.set("dispatch", prev_dispatch)?;
        ycode.set("actions", prev_actions)?;
        ycode.set("eval", prev_eval)?;
        ycode.set("settings", prev_settings)?;
        ycode.set("kv_get", prev_kv_get)?;
        ycode.set("kv_set", prev_kv_set)?;
        ycode.set("palette_hide", prev_palette_hide)?;
        ycode.set("spawn", prev_spawn)?;
        ycode.set("workspace_folders", prev_folders)?;
        ycode.set("open", prev_open_path)?;
        gpui.set("open_window", prev_open)?;
        result
    }

    /// Run every hook matching `name` as a `%orig` chain around `host.call_orig`.
    /// The most recently defined hook runs first. If a hook errors before calling
    /// `%orig`, the original still runs (unless this is a `%new` action).
    pub fn invoke(
        &self,
        name: &str,
        debug: &str,
        args: Option<Json>,
        host: &mut dyn Host,
    ) -> Result<Option<Json>> {
        self.refresh();
        let chain: Vec<Hook> = self
            .index
            .borrow()
            .iter()
            .filter(|h| pattern_matches(&h.name, name))
            .cloned()
            .collect();
        if chain.is_empty() {
            return host.call_orig(args);
        }
        let is_new = chain.iter().any(|h| h.is_new);
        let orig_calls = Cell::new(0u32);
        let host = RefCell::new(host);

        let result = self.lua.scope(|scope| {
            let lua = &self.lua;
            let args_val = match &args {
                Some(a) => lua.to_value(a)?,
                None => Value::Nil,
            };
            let mut orig = scope.create_function(|lua, a: Option<Value>| {
                orig_calls.set(orig_calls.get() + 1);
                let a = a.map(|v| lua.from_value::<Json>(v)).transpose()?;
                let r = host
                    .try_borrow_mut()
                    .map_err(|_| mlua::Error::runtime("%orig: host busy"))?
                    .call_orig(a)
                    .map_err(mlua::Error::external)?;
                r.map(|j| lua.to_value(&j)).transpose()
            })?;
            for hook in &chain {
                let me = lua.create_table()?;
                me.set("name", name)?;
                me.set("debug", debug)?;
                me.set("src", hook.src.as_str())?;
                me.set("args", args_val.clone())?;
                me.set("orig", orig.clone())?;
                let func = hook.func.clone();
                let default_args = args_val.clone();
                orig = scope.create_function(move |_, a: Option<Value>| {
                    func.call::<MultiValue>((me.clone(), a.unwrap_or(default_args.clone())))
                })?;
            }
            self.with_api(scope, &host, || orig.call::<Value>(args_val.clone()))
        });

        let host = host.into_inner();
        match result {
            Ok(v) => Ok(self.lua.from_value::<Json>(v).ok().filter(|j| !j.is_null())),
            Err(e) => {
                log::error!(target: "lua_hooks", "{name}: {e}");
                if orig_calls.get() == 0 && !is_new {
                    host.call_orig(args)?;
                }
                Err(lua_err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mock {
        orig: Vec<Option<Json>>,
        dispatched: Vec<(String, Option<Json>)>,
    }

    impl Host for Mock {
        fn call_orig(&mut self, args: Option<Json>) -> Result<Option<Json>> {
            self.orig.push(args);
            Ok(Some(Json::from("orig-result")))
        }
        fn dispatch(&mut self, name: &str, args: Option<Json>) -> Result<()> {
            self.dispatched.push((name.into(), args));
            Ok(())
        }
        fn action_names(&self) -> Vec<String> {
            vec!["editor::Copy".into()]
        }
    }

    fn rt(src: &str) -> Runtime {
        let rt = Runtime::new().unwrap();
        rt.load_str("t.x.lua", src, true).unwrap();
        rt
    }

    #[test]
    fn status_buttons_replace_by_id() {
        let rt = Runtime::new().unwrap();
        rt.load_str(
            "b.lua",
            "ycode.status_button{ id = 'm', action = 'a::B', icon = 'settings' }\n\
             ycode.status_button{ id = 'm', action = 'a::C', label = 'Mods' }",
            false,
        )
        .unwrap();
        let b = rt.status_buttons();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].action, "a::C");
        assert_eq!(b[0].label.as_deref(), Some("Mods"));
    }

    #[test]
    fn orig_chain_and_log() {
        let rt = rt(r#"
%hook editor
- Copy
    %log("outer-before")
    %orig
    %log("outer-after")
end
%end
%hook editor
- *
    %log("wild")
    return %orig
end
%end
"#);
        assert!(rt.wants("editor::Copy"));
        assert!(rt.wants("editor::Paste"));
        assert!(!rt.wants("workspace::Save"));
        let mut host = Mock::default();
        rt.invoke("editor::Copy", "Copy", None, &mut host).unwrap();
        assert_eq!(host.orig, vec![None]);
        // later definitions wrap earlier ones
        assert_eq!(
            rt.take_log(),
            vec![
                "editor::Copy wild",
                "editor::Copy outer-before",
                "editor::Copy outer-after"
            ]
        );
    }

    #[test]
    fn swallow_replace_and_dispatch() {
        let rt = rt(r#"
%hook editor
- Copy
    ycode.dispatch("editor::SelectAll", { foo = 1 })
end
- MoveDown
    %orig({ lines = 3 })
end
%end
"#);
        let mut host = Mock::default();
        rt.invoke("editor::Copy", "", None, &mut host).unwrap();
        assert!(host.orig.is_empty(), "not calling %orig swallows");
        assert_eq!(host.dispatched[0].0, "editor::SelectAll");
        assert_eq!(host.dispatched[0].1, Some(serde_json::json!({"foo": 1})));
        rt.invoke("editor::MoveDown", "", None, &mut host).unwrap();
        assert_eq!(host.orig, vec![Some(serde_json::json!({"lines": 3}))]);
    }

    #[test]
    fn groups_ctor_new_and_hookf() {
        let rt = rt(r#"
%group late
%hook pane
- Close
    return "hooked"
end
%end
%end

%hook ycode
%new - Shout(args)
    ycode.log("shout", args.text)
end
%end

%hookf app::point(a)
    return %orig({ n = a.n + 1 })
end

%ctor
    ycode.dispatch("ycode::Shout", { text = "hi" })
    %init(late)
end
"#);
        assert!(!rt.wants("pane::Close"));
        let mut host = Mock::default();
        assert!(rt.run_ctors(&mut host).is_empty());
        assert!(rt.wants("pane::Close"));
        assert!(rt.is_new("ycode::Shout"));
        assert_eq!(
            host.dispatched,
            vec![(
                "ycode::Run".into(),
                Some(serde_json::json!({"name": "ycode::Shout", "args": {"text": "hi"}}))
            )]
        );
        let r = rt.invoke("pane::Close", "", None, &mut host).unwrap();
        assert_eq!(r, Some(Json::from("hooked")));

        rt.invoke(
            "ycode::Shout",
            "",
            Some(serde_json::json!({"text": "yo"})),
            &mut host,
        )
        .unwrap();
        assert_eq!(rt.take_log(), vec!["shout yo"]);

        let r = rt
            .invoke(
                "app::point",
                "",
                Some(serde_json::json!({"n": 1})),
                &mut host,
            )
            .unwrap();
        assert_eq!(r, Some(Json::from("orig-result")));
        assert_eq!(
            host.orig.last().unwrap(),
            &Some(serde_json::json!({"n": 2}))
        );
    }

    #[test]
    fn errors_fail_open() {
        let rt = rt("%hook editor\n- Copy\n  error('boom')\nend\n%end\n");
        let mut host = Mock::default();
        let err = rt.invoke("editor::Copy", "", None, &mut host).unwrap_err();
        assert!(err.to_string().contains("t.x.lua:3: boom"), "{err}");
        assert_eq!(host.orig, vec![None]);
        assert!(
            Runtime::new()
                .unwrap()
                .load_str("bad.x.lua", "%orig", true)
                .unwrap_err()
                .to_string()
                .contains("bad.x.lua:1")
        );
        // API is unbound outside hooks
        let rt = Runtime::new().unwrap();
        assert!(rt.load_str("x.lua", "ycode.dispatch('a')", false).is_err());
    }

    #[test]
    fn example_file() {
        let rt = Runtime::new().unwrap();
        rt.load_str(
            "example.x.lua",
            include_str!("../hooks/example.x.lua"),
            true,
        )
        .unwrap();
        let mut host = Mock::default();
        assert!(rt.run_ctors(&mut host).is_empty());
        rt.invoke("workspace::Save", "Save", None, &mut host)
            .unwrap();
        assert!(!rt.wants("pane::CloseActiveItem"));
        rt.invoke("ycode::Run", "", None, &mut host).unwrap();
        rt.invoke("editor::SelectAllAndCopy", "", None, &mut host)
            .unwrap();
        assert_eq!(host.orig, vec![None, None]);
        assert_eq!(
            host.dispatched,
            vec![
                ("editor::SelectAll".into(), None),
                ("editor::Copy".into(), None)
            ]
        );
        assert_eq!(
            rt.take_log(),
            vec![
                "example hooks loaded; 1 actions known",
                "workspace::Save saving",
                "workspace::Save saved"
            ]
        );
    }

    #[test]
    fn full_stdlib_and_c_loader() {
        let rt = Runtime::new().unwrap();
        // safe mode has no `debug` and refuses C modules with "safe mode" errors
        rt.load_str("t.lua", "assert(debug.getinfo(1, 'S'))", false)
            .unwrap();
        let err = rt
            .load_str("t.lua", "require('definitely_missing_c_module')", false)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("safe mode"), "{err}");
        assert!(err.contains("no file"), "{err}");
    }

    #[test]
    fn luasocket_and_print() {
        let rt = Runtime::new().unwrap();
        rt.load_str(
            "t.lua",
            "local http = require('socket.http'); local ltn12 = require('ltn12'); print(type(http.request), type(ltn12.sink.table), type(require('socket').connect))",
            false,
        )
        .unwrap();
        let log = rt.take_log();
        assert_eq!(log.len(), 1);
        assert!(log[0] == "function function function", "{log:?}");
    }

    #[test]
    fn settings_merge_and_extensions() {
        #[derive(Default)]
        struct SettingsHost(Vec<Json>);
        impl Host for SettingsHost {
            fn call_orig(&mut self, _: Option<Json>) -> Result<Option<Json>> {
                Ok(None)
            }
            fn dispatch(&mut self, _: &str, _: Option<Json>) -> Result<()> {
                Ok(())
            }
            fn action_names(&self) -> Vec<String> {
                Vec::new()
            }
            fn set_settings(&mut self, s: Json) -> Result<()> {
                self.0.push(s);
                Ok(())
            }
        }
        let dir = std::env::temp_dir().join(format!("lua_hooks_ext_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("ext/lib")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join("a.x.lua"),
            "%ctor\n ycode.settings{ title_bar = { show_sign_in = false } }\nend",
        )
        .unwrap();
        std::fs::write(dir.join("ext/b.x.lua"), "%ctor\n ycode.settings{ title_bar = { show_user_picture = false }, auto_update = false }\nend").unwrap();
        std::fs::write(
            dir.join("ext/lib/m.lua"),
            "error('modules must not be loaded as hooks')",
        )
        .unwrap();
        std::fs::write(dir.join(".git/c.lua"), "error('hidden dirs are skipped')").unwrap();
        let rt = Runtime::new().unwrap();
        assert!(rt.load_dir(&dir).is_empty());
        let mut host = SettingsHost::default();
        assert!(rt.run_ctors(&mut host).is_empty());
        assert_eq!(
            host.0.last().unwrap(),
            &serde_json::json!({"title_bar": {"show_sign_in": false, "show_user_picture": false}, "auto_update": false})
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn json_round_trip() {
        let rt = Runtime::new().unwrap();
        rt.load_str(
            "t.lua",
            r#"local t = ycode.json_decode('{"a": [1, 2], "b": {"c": "x"}}')
               print(t.a[2], t.b.c, ycode.json_encode({ k = true }))"#,
            false,
        )
        .unwrap();
        assert_eq!(rt.take_log(), vec![r#"2 x {"k":true}"#]);
    }

    #[test]
    fn executor_file_loads() {
        let rt = Runtime::new().unwrap();
        rt.load_str(
            "executor.x.lua",
            include_str!("../hooks/executor.x.lua"),
            true,
        )
        .unwrap();
        assert!(rt.is_new("ycode::Executor"));
    }

    #[test]
    fn eval_is_bound_in_hooks() {
        let rt = rt(
            "%hook t\n- A\n  local ok, r = ycode.eval('return 6*7')\n  ycode.log(ok, r)\n  local ok2, e = ycode.eval('error(\"x\")')\n  ycode.log(ok2)\nend\n%end",
        );
        let mut host = Mock::default();
        rt.invoke("t::A", "", None, &mut host).unwrap();
        assert_eq!(rt.take_log(), vec!["true 42", "false"]);
    }

    #[test]
    fn plain_lua_api() {
        let rt = Runtime::new().unwrap();
        rt.load_str(
            "p.lua",
            "ycode.hook('editor::Copy', function(self) ycode.log(#ycode.actions()); return self.orig() end)",
            false,
        )
        .unwrap();
        let mut host = Mock::default();
        rt.invoke("editor::Copy", "", None, &mut host).unwrap();
        assert_eq!(rt.take_log(), vec!["1"]);
        assert_eq!(host.orig.len(), 1);
    }
}
