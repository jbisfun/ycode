# ycode

ycode is a fork of [Zed](https://github.com/zed-industries/zed) that can be scripted in
Lua. Its own code is a hook system, [`crates/lua_hooks`](crates/lua_hooks). It lets
plugins written in Lua intercept and replace any Zed action, add new actions and draw
native gpui windows and dock panels. The rest of the editor is upstream Zed with a few
small patches.

ycode is part of the [jbisfun](https://github.com/jbisfun) workspace, which is managed
with [pull](https://github.com/jbisfun/pull). Most features live in plugins, not in this
repo.

## Lua hooks

Hook files are `~/.config/zed/hooks/*.lua` and `*.x.lua`, plus one level of extension
subdirectories (`hooks/<ext>/`). `.x.lua` files use directives modelled on
[Logos](https://theos.dev/docs/logos-syntax):

```lua
%hook editor
- Copy
  %log
  %orig
end
%end
```

- `%hook ns` / `- Action`: wrap `ns::Action` (`- *` wraps the whole namespace). If a
  hook never calls `%orig`, the action is swallowed.
- `%new - Name(args)`: a new action, shown in the command palette.
- `%hookf point`: wrap a hook point in the Rust code (e.g. `client::authenticate`, the
  automatic sign-in at startup).
- `%ctor`, `%group`/`%init`, `%log`.

The Lua API (`ycode.*`, `gpui.*`) covers dispatching actions, a settings layer, the KV
store, status-bar buttons, workspace folders, spawning processes and native UI elements
(`gpui.open_window`, `gpui.register_panel`). LuaSocket comes embedded. Plugins are
reloaded with `ycode: reload hooks` and never need a rebuild.

- [crates/lua_hooks/README.md](crates/lua_hooks/README.md): directives and API
- [crates/lua_hooks/UI.md](crates/lua_hooks/UI.md): how Lua windows and panels should look
- `crates/lua_hooks/ycode-lua`: REPL/runner for the live executor socket
  (`$XDG_RUNTIME_DIR/ycode-lua.sock`)

## Plugins

Each plugin is its own repo, symlinked into `~/.config/zed/hooks/<name>/`:

- [ycode-pull](https://github.com/jbisfun/ycode-pull): pull's multi-repo Pull panel,
  sessions and jobs. It replaces Zed's git panel, which handles only one repo.
- [ycode-hermes](https://github.com/jbisfun/ycode-hermes): Hermes Agent as the agent,
  over ACP
- [ycode-idl](https://github.com/jbisfun/ycode-idl): panel for the idl CLI
- [ycode-manager](https://github.com/jbisfun/ycode-manager): enable/disable plugins
- [ycode-offline](https://github.com/jbisfun/ycode-offline): turns off sign-in and the
  online features

## Changes to Zed

Outside `crates/lua_hooks`, the patches are small and generic:

- gpui: `App::intercept_actions`, a single slot that wraps every window and global
  action dispatch
- zed: `lua_hooks::init` runs after all other inits. Startup sign-in goes through the
  `client::authenticate` hook point. The Lua C API is exported so `require` can load C
  modules.
- settings: a plugin settings layer (`SettingsStore::set_plugin_settings`)
- command palette: `%new` actions appear in the palette, and actions can be hidden
- workspace: Lua dock panels (`panel_index_for_id`)
- agent_ui: `agent::SyncAgentThreads` imports an ACP agent's existing sessions

## Building

The build is the same as upstream Zed ([Linux](docs/src/development/linux.md),
[macOS](docs/src/development/macos.md), [Windows](docs/src/development/windows.md)):

```sh
cargo build -p zed        # target/debug/zed
```

## License

ycode is licensed like upstream Zed: GPL-3.0-or-later, with Apache-2.0 components where
marked. See `LICENSE-GPL` and `LICENSE-APACHE`.
