# lua_hooks

Lua (mlua, Lua 5.4) hooks for ycode, using syntax modelled on Logos.

## How it hooks in

- `gpui::App::intercept_actions` wraps every action dispatch, in a window or global. That
  covers keybindings, menus, the command palette and `dispatch_action`. Hooks run there.
- `zed`'s `main()` calls `lua_hooks::init(cx)` after all other `init`s, so all actions
  exist before hook files load.
- Hook files load from `~/.config/zed/hooks/` in name order. `ycode: reload hooks`
  (`ycode::ReloadHooks`) reloads them. Errors go to the Zed log under target `lua_hooks`.

## Directives (`*.x.lua`)

| Logos               | ycode                         | meaning                                         |
|---------------------|-------------------------------|-------------------------------------------------|
| `%hook Class`       | `%hook editor`                | action namespace                                |
| `- (void)sel { }`   | `- Copy` ... `end`            | hook `editor::Copy`; `- *` hooks the whole namespace |
| `%orig` / `%orig(x)`| same                          | run the original, or the action rebuilt from args `x` |
| `%log`              | `%log` / `%log(a, b)`         | `ycode.log(self.name, a, b)`                    |
| `%new`              | `%new - Name(args)`           | new action, invoked via `ycode::Run`            |
| `%group` / `%init`  | same                          | hooks inactive until `%init(group)`             |
| `%ctor { }`         | `%ctor` ... `end`             | runs once after all files load                  |
| `%hookf`            | `%hookf app::point(args)`     | hook a Rust hook point (`lua_hooks::call_point`) |
| `%end`              | same                          | close `%hook` / `%group`                        |

Method bodies are plain Lua closed by Lua's own `end`. Output keeps line numbers, so Lua
errors point at the source line. Inside a hook, `self` has `name`, `debug` (the action's
Debug form), `args`, `src` and `orig`.

- If a hook never calls `%orig`, the action is swallowed.
- If several hooks match, the one defined last runs first.
- If a hook errors before calling `%orig`, the original still runs.

## Lua API

- `ycode.log(...)`
- `ycode.dispatch(name, args?)` (deferred; also runs `%new` actions)
- `ycode.actions()`
- `ycode.init(group)`
- `ycode.hook(name, fn(self, args))`
- `ycode.new_action(name, fn)`
- `ycode.ctor(fn)`
- `ycode.hooks_dir`: the hooks directory (string).
- `ycode.open(path)`: open a file in the workspace.
- `gpui.register_panel{ id, title?, icon?, position?, size?, render, state?, poll_ms?,
  on_key?, focus? }`: a dock panel rendered like `gpui.open_window` (same elements);
  `position` is `left` (default), `right` or `bottom`. Toggle it with
  `["ycode::TogglePanel", { "id": ... }]`; `w.close()` hides the dock. Panels are rebuilt
  in every workspace on reload.
- `ycode.workspace_folders()`: folders open in workspace windows, the active window's
  first.
- `ycode.status_button{ id, action, args?, icon?, label?, tooltip? }`: a button in every
  workspace's status bar that dispatches `action` (a `%new` action or any action);
  `icon` is a snake_case `IconName` (e.g. `"blocks"`), otherwise `label` is shown. The
  same `id` replaces a button; buttons go away when their mod is unloaded.

`ycode.dispatch` and `ycode.actions` only work inside a hook or ctor.

See `hooks/example.x.lua`.
