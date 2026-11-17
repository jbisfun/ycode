-- Example ycode hooks. Copy into ~/.config/zed/hooks/ and run "ycode: reload hooks".
-- Files ending in .x.lua use the Logos-style directives below; plain .lua files are
-- loaded as-is and can use ycode.hook / ycode.new_action / ycode.ctor instead.

%ctor
    ycode.log("example hooks loaded;", #ycode.actions(), "actions known")
end

%hook workspace
- Save
    %log("saving")
    %orig                          -- run the real workspace::Save
    %log("saved")
end
%end

%hook editor
- Copy
    %log(self.debug)               -- self.name, self.debug, self.args, self.orig
    %orig
end

-- A new action. Bind it in keymap.json:
--   "ctrl-alt-c": ["ycode::Run", { "name": "editor::SelectAllAndCopy" }]
%new - SelectAllAndCopy
    ycode.dispatch("editor::SelectAll")
    ycode.dispatch("editor::Copy")
end
%end

-- Inactive until %init(paranoid) runs, e.g. from a %ctor.
%group paranoid
%hook pane
- CloseActiveItem
    %log("blocked")                -- not calling %orig swallows the action
end
%end
%end
