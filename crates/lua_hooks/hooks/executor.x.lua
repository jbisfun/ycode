-- The Lua executor window, written in Lua on top of gpui.open_window.
-- Open it from the command palette ("ycode: executor") or bind
-- ["ycode::Run", { "name": "ycode::Executor" }].
--
-- ctrl-enter runs the input; ctrl-up / ctrl-down walk history. Input can be plain Lua or
-- hook directives (%hook ... %end); expressions print their value. Hook logs stream in.

local kinds = {
    input  = { prefix = "> ", fg = "accent" },
    result = { prefix = "= ", fg = "text" },
    error  = { prefix = "! ", fg = "error" },
    log    = { prefix = "· ", fg = "muted" },
}

local function push(w, kind, text)
    local lines = w.state.lines
    lines[#lines + 1] = { kind = kind, text = tostring(text) }
    if #lines > 2000 then table.remove(lines, 1) end
    w.state.stick = true
end

local function drain_log(w)
    for _, line in ipairs(ycode.take_log()) do push(w, "log", line) end
end

local function run(w)
    local code = w.text("code")
    if not code or code:match("^%s*$") then return end
    push(w, "input", code)
    local h = w.state.history
    if h[#h] ~= code then h[#h + 1] = code end
    w.state.hist = nil
    local ok, result = ycode.eval(code)
    drain_log(w)
    if not ok then
        push(w, "error", result)
    elseif result ~= "" then
        push(w, "result", result)
    end
    w.set_text("code", "")
end

local function history(w, step)
    local h = w.state.history
    if #h == 0 then return end
    local i = (w.state.hist or #h + 1) + step
    if i < 1 then i = 1 end
    if i > #h then
        w.state.hist = nil
        w.set_text("code", "")
        return
    end
    w.state.hist = i
    w.set_text("code", h[i])
end

local function on_key(w, ev)
    if ev.ctrl and ev.key == "enter" then
        run(w)
        return true
    elseif ev.ctrl and ev.key == "up" then
        history(w, -1)
        return true
    elseif ev.ctrl and ev.key == "down" then
        history(w, 1)
        return true
    end
end

local function render(w)
    drain_log(w)
    if w.state.stick then
        w.state.stick = false
        w.scroll_bottom("output")
    end

    local output = { "col", id = "output", scroll = true, grow = true, p = 10, gap = 2, mono = true, size = "small" }
    for _, line in ipairs(w.state.lines) do
        local k = kinds[line.kind]
        output[#output + 1] = { "text", k.prefix .. line.text, fg = k.fg }
    end

    return { "col", w = "full", h = "full",
        output,
        { "col", p = 8, gap = 6, bg = "panel", border = "border",
            { "input", id = "code", multiline = true, h = 120, mono = true,
              placeholder = "Lua or %hook … %end   (ctrl-enter: run, ctrl-↑/↓: history)" },
            { "row", gap = 6, align = "center",
                { "button", "Run", on_click = run },
                { "button", "Clear", on_click = function(w) w.state.lines = {} end },
                { "button", "Reload hooks", on_click = function()
                    ycode.dispatch("ycode::ReloadHooks")
                    ycode.dispatch("ycode::Executor")
                end },
                { "row", grow = true },
                { "text", #w.state.history .. " in history", fg = "muted", size = "small" },
            },
        },
    }
end

%hook ycode
%new - Executor
    gpui.open_window{
        title = "ycode Lua executor",
        width = 820, height = 600,
        focus = "code",
        poll_ms = 400,
        state = {
            lines = { { kind = "log", text = "ycode Lua executor. ctrl-enter runs; try: %hook editor / - Copy / %log(self.debug) / %orig / end / %end" } },
            history = {},
        },
        on_key = on_key,
        render = render,
    }
end
%end
