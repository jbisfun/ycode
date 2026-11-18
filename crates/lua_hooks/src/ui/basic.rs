//! Leaf elements: text, buttons, icons and toggles, each a native `ui` component.

use super::LuaWindow;
use super::props::{self, Props, Toggle, rt_err};
use gpui::{AnyElement, App, Context, IntoElement, SharedString, Window};
use mlua::{Table, Value};
use ui::{
    ButtonCommon as _, Checkbox, Clickable as _, Disableable as _, Disclosure, Divider,
    DividerColor, Icon, IconSize, Label, LabelCommon as _, LabelSize, ToggleButtonGroup,
    ToggleButtonSimple, ToggleState, Tooltip,
};

impl LuaWindow {
    /// `text` is a `ui::Label`: native size and color, UI font unless `mono`.
    pub(super) fn build_text(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let s = Self::text_of(t, start)?.or_else(|| p.label.clone()).unwrap_or_default();
        let label = Self::label(s, p, None, cx)?;
        // Clickable text (file links) gets a pointer and an underline on hover.
        match Self::callback(t, "on_click")? {
            None => Ok(label.into_any_element()),
            Some(f) => {
                use gpui::{InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _, Styled as _};
                let id = self.id_or_auto(p, "text");
                Ok(gpui::div()
                    .id(SharedString::from(id))
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(self.click(f, cx))
                    .child(label)
                    .into_any_element())
            }
        }
    }

    /// A `ui::Label` styled from `size color mono bold truncate`.
    pub(super) fn label(
        s: impl Into<SharedString>,
        p: &Props,
        default_size: Option<LabelSize>,
        cx: &App,
    ) -> mlua::Result<Label> {
        let mut label = Label::new(s);
        match &p.size {
            Some(size) => label = label.size(props::label_size(size)?),
            None => {
                if let Some(size) = default_size {
                    label = label.size(size);
                }
            }
        }
        if let Some(c) = &p.color {
            label = label.color(props::color(c, cx)?);
        }
        if p.mono {
            label = label.buffer_font(cx);
        }
        if p.bold {
            label = label.weight(gpui::FontWeight::SEMIBOLD);
        }
        if p.truncate {
            label = label.truncate();
        }
        Ok(label)
    }

    /// `button` (`ui::Button`, small label, subtle by default) and
    /// `icon_button` (`ui::IconButton`, small icon).
    pub(super) fn build_button(
        &mut self,
        tag: &str,
        t: &Table,
        p: &Props,
        start: usize,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let id = self.id_or_auto(p, tag);
        let click = Self::callback(t, "on_click")?.map(|f| self.click(f, cx));
        let icon = p.icon.as_deref().map(props::icon_name).transpose()?;
        let style = match p.style.as_deref() {
            None | Some("subtle") => ui::ButtonStyle::Subtle,
            Some("filled") => ui::ButtonStyle::Filled,
            Some("transparent") => ui::ButtonStyle::Transparent,
            Some(other) => {
                return Err(rt_err(format!("bad style {other:?}, expected subtle|filled|transparent")));
            }
        };

        if tag == "icon_button" {
            let icon = icon.ok_or_else(|| rt_err("needs an `icon`"))?;
            let mut btn = ui::IconButton::new(id, icon)
                .icon_size(IconSize::Small)
                .style(style)
                .disabled(p.disabled);
            if let Some(color) = &p.color {
                btn = btn.icon_color(props::color(color, cx)?);
            }
            if let Some(tt) = p.tooltip.clone() {
                btn = btn.tooltip(Tooltip::text(tt));
            }
            if let Some(f) = click {
                btn = btn.on_click(f);
            }
            return Ok(btn.into_any_element());
        }

        let label = Self::text_of(t, start)?.or_else(|| p.label.clone()).unwrap_or_default();
        let mut btn = ui::Button::new(id, label)
            .label_size(LabelSize::Small)
            .style(style)
            .disabled(p.disabled);
        if let Some(icon) = icon {
            btn = btn.start_icon(Icon::new(icon).size(IconSize::Small));
        }
        if let Some(color) = &p.color {
            btn = btn.color(props::color(color, cx)?);
        }
        if let Some(tt) = p.tooltip.clone() {
            btn = btn.tooltip(Tooltip::text(tt));
        }
        if let Some(f) = click {
            btn = btn.on_click(f);
        }
        Ok(btn.into_any_element())
    }

    /// `icon{ name, size, color }` (`ui::Icon`, small by default).
    pub(super) fn build_icon(p: &Props, cx: &App) -> mlua::Result<AnyElement> {
        let name = p.name.as_deref().or(p.icon.as_deref()).ok_or_else(|| rt_err("needs a `name`"))?;
        let mut icon = Icon::new(props::icon_name(name)?).size(match &p.size {
            Some(s) => props::icon_size(s)?,
            None => IconSize::Small,
        });
        if let Some(c) = &p.color {
            icon = icon.color(props::color(c, cx)?);
        }
        Ok(icon.into_any_element())
    }

    /// `checkbox{ checked = true|false|"mixed", label, size, disabled, on_click }`.
    pub(super) fn build_checkbox(
        &mut self,
        t: &Table,
        p: &Props,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let checked = match &p.checked {
            None | Some(Toggle::Bool(false)) => ToggleState::Unselected,
            Some(Toggle::Bool(true)) => ToggleState::Selected,
            Some(Toggle::Str(s)) if s == "mixed" => ToggleState::Indeterminate,
            Some(other) => {
                return Err(rt_err(format!("bad checked {other:?}, expected true|false|\"mixed\"")));
            }
        };
        let id = self.id_or_auto(p, "checkbox");
        let mut checkbox = Checkbox::new(SharedString::from(id), checked).disabled(p.disabled);
        if let Some(label) = p.label.clone() {
            let size = match &p.size {
                Some(s) => props::label_size(s)?,
                None => LabelSize::Small,
            };
            checkbox = checkbox.label(label).label_size(size);
        }
        if let Some(f) = Self::callback(t, "on_click")? {
            let entity = cx.entity();
            checkbox = checkbox.on_click(move |_, window, cx| {
                entity.update(cx, |this, cx| {
                    this.event(&f, Value::Nil, window, cx);
                });
            });
        }
        Ok(checkbox.into_any_element())
    }

    /// `toggle_group{ options, selected, on_select = fn(w, option) }`
    /// (`ui::ToggleButtonGroup`, 1-6 options).
    pub(super) fn build_toggle_group(
        &mut self,
        t: &Table,
        p: &Props,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let options = p.options.clone().unwrap_or_default();
        if options.is_empty() {
            return Err(rt_err("needs an `options` list"));
        }
        let selected_index = match &p.selected {
            Some(Toggle::Str(s)) => options.iter().position(|o| o == s).unwrap_or(0),
            _ => 0,
        };
        let group_name = SharedString::from(self.id_or_auto(p, "toggle-group"));
        let on_select = Self::callback(t, "on_select")?;
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
                    let Ok(value) = rt.lua().create_string(&opt) else { return };
                    entity.update(cx, |this, cx| {
                        this.event(&f, Value::String(value), window, cx);
                    });
                })
            })
            .collect();
        // ToggleButtonGroup is generic over a const column count.
        macro_rules! group {
            ($n:literal) => {{
                let arr: [ToggleButtonSimple; $n] = match buttons.try_into() {
                    Ok(arr) => arr,
                    Err(_) => unreachable!(),
                };
                ToggleButtonGroup::single_row(group_name, arr)
                    .selected_index(selected_index)
                    .auto_width()
                    .label_size(LabelSize::Small)
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
            n => return Err(rt_err(format!("supports 1-6 options, got {n}"))),
        })
    }

    /// `disclosure{ open, on_click }` (`ui::Disclosure` chevron).
    pub(super) fn build_disclosure(
        &mut self,
        t: &Table,
        p: &Props,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let id = self.id_or_auto(p, "disclosure");
        let mut disclosure = Disclosure::new(SharedString::from(id), p.open);
        if let Some(f) = Self::callback(t, "on_click")? {
            let handler: std::sync::Arc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)> =
                std::sync::Arc::new(self.click(f, cx));
            disclosure = disclosure.on_toggle_expanded(handler);
        }
        Ok(disclosure.into_any_element())
    }

    /// `divider{ vertical }` (`ui::Divider`).
    pub(super) fn build_divider(p: &Props) -> AnyElement {
        let divider = if p.vertical { Divider::vertical() } else { Divider::horizontal() };
        divider.color(DividerColor::BorderVariant).into_any_element()
    }
}
