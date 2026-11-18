//! List elements: `list_item`, `list_header` and `section`.

use super::LuaWindow;
use super::props::Props;
use gpui::{AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div};
use mlua::Table;
use ui::{LabelCommon as _, LabelSize, ListHeader, ListItem, ListItemSpacing, ListSubHeader, Toggleable as _};

impl LuaWindow {
    /// `list_item{ "label" | children, detail, selected, indent, start, end_,
    /// end_hover, on_click }`: a `ui::ListItem` (dense, inset, rounded hover).
    pub(super) fn build_list_item(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let id = self.id_or_auto(p, "list-item");
        let start_slot = self.slot(t, "start", window, cx)?;
        let end_slot = self.slot(t, "end_", window, cx)?;
        let end_hover = self.slot(t, "end_hover", window, cx)?;
        let selected = p.selected.as_ref().is_some_and(|s| s.is_true());
        let mut item = ListItem::new(SharedString::from(id))
            .spacing(ListItemSpacing::Dense)
            .inset(true)
            .indent_level(p.indent.unwrap_or(0))
            .toggle_state(selected)
            .start_slot::<AnyElement>(start_slot)
            .end_slot::<AnyElement>(end_slot);
        if let Some(h) = end_hover {
            item = item.end_slot_on_hover(h);
        }
        if let Some(tt) = p.tooltip.clone() {
            item = item.tooltip(ui::Tooltip::text(tt));
        }
        if let Some(f) = Self::callback(t, "on_click")? {
            item = item.on_click(self.click(f, cx));
        }
        // A string label (plus optional detail) reads like a native row; other
        // children are laid out as given.
        if let Some(label) = Self::text_of_strings(t, start)? {
            let mut row = ui::h_flex()
                .min_w_0()
                .gap(ui::DynamicSpacing::Base06.rems(cx))
                .child(Self::label(label, p, None, cx)?.truncate());
            if let Some(detail) = p.detail.clone() {
                row = row.child(
                    ui::Label::new(detail)
                        .size(LabelSize::Small)
                        .color(ui::Color::Muted)
                        .truncate(),
                );
            }
            item = item.child(row);
        } else {
            item = item.children(self.children(t, start, window, cx)?);
        }
        Ok(item.into_any_element())
    }

    /// The positional children if they are all plain strings/numbers.
    fn text_of_strings(t: &Table, start: usize) -> mlua::Result<Option<String>> {
        let len = t.raw_len();
        if start > len {
            return Ok(None);
        }
        for i in start..=len {
            if matches!(t.raw_get::<mlua::Value>(i)?, mlua::Value::Table(_)) {
                return Ok(None);
            }
        }
        Self::text_of(t, start)
    }

    /// `list_header{ "label", sub, start, end_, end_hover, on_click }`:
    /// `ui::ListHeader`, or `ui::ListSubHeader` with `sub = true`.
    pub(super) fn build_list_header(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let label = Self::text_of(t, start)?.or_else(|| p.label.clone()).unwrap_or_default();
        let end_slot = self.slot(t, "end_", window, cx)?;
        let header = if p.sub {
            let mut header = ListSubHeader::new(label).inset(true);
            if let Some(end_slot) = end_slot {
                header = header.end_slot(end_slot);
            }
            header.into_any_element()
        } else {
            let start_slot = self.slot(t, "start", window, cx)?;
            let end_hover = self.slot(t, "end_hover", window, cx)?;
            let mut header = ListHeader::new(label)
                .inset(true)
                .start_slot::<AnyElement>(start_slot)
                .end_slot::<AnyElement>(end_slot)
                .end_hover_slot::<AnyElement>(end_hover);
            if let Some(f) = Self::callback(t, "on_click")? {
                let click = self.click(f, cx);
                header = header.on_toggle(click);
            }
            header.into_any_element()
        };
        // Native headers are flex_1; in a Lua column that stretches them.
        Ok(div().flex_none().w_full().child(header).into_any_element())
    }

    /// `section{ "Title", action, empty, children }`: a sub-header and its rows.
    pub(super) fn build_section(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let mut title: Option<String> = p.title.clone();
        let mut first = start;
        if title.is_none() {
            if let mlua::Value::String(s) = t.raw_get::<mlua::Value>(start)? {
                title = Some(s.to_string_lossy());
                first = start + 1;
            }
        }
        let mut col = ui::v_flex().flex_none().w_full();
        if let Some(title) = title {
            let mut header = ListSubHeader::new(title).inset(true);
            if let Some(action) = self.slot(t, "action", window, cx)? {
                header = header.end_slot(action);
            }
            col = col.child(div().flex_none().w_full().child(header));
        }
        let rows = self.children(t, first, window, cx)?;
        if rows.is_empty() {
            if let Some(empty) = p.empty.clone() {
                // Wraps (errors can be long); indented like list_item labels.
                col = col.child(
                    div()
                        .w_full()
                        .pl_2p5()
                        .pr_2()
                        .py(ui::DynamicSpacing::Base02.rems(cx))
                        .child(
                            ui::Label::new(empty)
                                .size(LabelSize::Small)
                                .color(ui::Color::Placeholder),
                        ),
                );
            }
        }
        Ok(col.children(rows).pb(ui::DynamicSpacing::Base04.rems(cx)).into_any_element())
    }
}
