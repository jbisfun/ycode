//! Containers: `panel`, `panel_header`, `toolbar`, `split`, `empty`, `col`/`row`
//! and `input`.

use super::LuaWindow;
use super::props::{self, Len, Props, rt_err};
use editor::Editor;
use gpui::{
    AnyElement, AppContext as _, Context, Div, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled,
    Window, div, relative,
};
use mlua::{Table, Value};
use theme::ActiveTheme as _;
use ui::{Color, DynamicSpacing, Label, LabelCommon as _, LabelSize, Tab, WithScrollbar as _};

impl LuaWindow {
    /// Makes `el` a vertical scroll area with a native scrollbar. The scroll
    /// position lives under `id`, so it survives re-renders.
    fn scrollable(
        &mut self,
        el: Div,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self.scrolls.entry(id.clone()).or_default().clone();
        el.id(SharedString::from(id))
            .overflow_y_scroll()
            .track_scroll(&handle)
            .vertical_scrollbar_for(&handle, window, cx)
            .into_any_element()
    }

    /// `panel{ header, footer, scroll, children }`: header, a body that fills
    /// the rest (scrolling unless `scroll = false`), and a footer line.
    pub(super) fn build_panel(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let mut root = ui::v_flex().size_full().min_h_0();
        match t.get::<Value>("header")? {
            Value::Nil => {}
            Value::String(s) => {
                root = root.child(self.panel_header(s.to_string_lossy(), None, Vec::new(), cx));
            }
            Value::Table(h) => {
                let hp = Props::parse(self.rt.lua(), &h).map_err(|e| rt_err(format!("header: {e}")))?;
                let title = hp.title.clone().or(Self::text_of(&h, 1)?).unwrap_or_default();
                let actions = self.slot_list(&h, "actions", window, cx)?;
                root = root.child(self.panel_header(title, hp.context.clone(), actions, cx));
            }
            other => return Err(rt_err(format!("bad header {}", other.type_name()))),
        }

        let mut body = ui::v_flex().flex_1().min_h_0().w_full();
        if let Some(gap) = &p.gap {
            body = body.gap(props::spacing(gap, "gap", cx)?);
        }
        let children = self.children(t, start, window, cx)?;
        let body = if p.scroll.unwrap_or(true) {
            let id = p.id.clone().unwrap_or_else(|| "panel-body".into());
            self.scrollable(body.py(DynamicSpacing::Base04.rems(cx)).children(children), id, window, cx)
        } else {
            body.children(children).into_any_element()
        };
        root = root.child(body);

        let footer = match t.get::<Value>("footer")? {
            Value::Nil => None,
            Value::String(s) => Some(
                Self::muted_label(s.to_string_lossy(), p, cx)?.truncate()
                    .into_any_element(),
            ),
            v => Some(self.build(v, window, cx)),
        };
        if let Some(footer) = footer {
            root = root.child(
                ui::h_flex()
                    .flex_none()
                    .w_full()
                    .min_w_0()
                    .gap(DynamicSpacing::Base04.rems(cx))
                    .px(DynamicSpacing::Base08.rems(cx))
                    .py(DynamicSpacing::Base04.rems(cx))
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(footer),
            );
        }
        Ok(root.into_any_element())
    }

    /// Small muted label (unless `size`/`color` say otherwise).
    fn muted_label(s: String, p: &Props, cx: &gpui::App) -> mlua::Result<Label> {
        let label = Self::label(s, p, Some(LabelSize::Small), cx)?;
        Ok(if p.color.is_none() { label.color(Color::Muted) } else { label })
    }

    fn panel_header(
        &self,
        title: String,
        context: Option<String>,
        actions: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        ui::h_flex()
            .flex_none()
            .w_full()
            .h(Tab::container_height(cx))
            .justify_between()
            .gap(DynamicSpacing::Base04.rems(cx))
            .pl(DynamicSpacing::Base08.rems(cx))
            .pr(DynamicSpacing::Base04.rems(cx))
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                ui::h_flex()
                    .min_w_0()
                    .gap(DynamicSpacing::Base06.rems(cx))
                    .child(Label::new(title).truncate())
                    .children(context.map(|c| {
                        Label::new(c).size(LabelSize::Small).color(Color::Muted).truncate()
                    })),
            )
            .child(ui::h_flex().flex_none().gap(DynamicSpacing::Base02.rems(cx)).children(actions))
            .into_any_element()
    }

    /// `panel_header{ title, context, actions | children }`.
    pub(super) fn build_panel_header(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let mut actions = self.slot_list(t, "actions", window, cx)?;
        actions.extend(self.children(t, start, window, cx)?);
        let title = p.title.clone().unwrap_or_default();
        Ok(self.panel_header(title, p.context.clone(), actions, cx))
    }

    /// `toolbar{ children }`: a wrapping row of buttons.
    pub(super) fn build_toolbar(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let mut row = ui::h_flex()
            .flex_none()
            .flex_wrap()
            .w_full()
            .gap(DynamicSpacing::Base02.rems(cx))
            .px(DynamicSpacing::Base06.rems(cx))
            .py(DynamicSpacing::Base02.rems(cx));
        row = Self::style(row, p, cx)?;
        Ok(row.children(self.children(t, start, window, cx)?).into_any_element())
    }

    /// `split{ a, b, ratio, vertical }`: two panes with a divider.
    pub(super) fn build_split(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let mut panes = self.children(t, start, window, cx)?.into_iter();
        let (Some(a), Some(b), None) = (panes.next(), panes.next(), panes.next()) else {
            return Err(rt_err("needs exactly two children"));
        };
        let pane = || ui::v_flex().min_w_0().min_h_0().overflow_hidden();
        let (first, second) = if p.vertical {
            let r = p.ratio.unwrap_or(0.5);
            (pane().w_full().h(relative(r)).flex_none(), pane().w_full().flex_1())
        } else {
            let first = match p.ratio {
                Some(r) => pane().h_full().w(relative(r)).flex_none(),
                // A sidebar, but never more than about half of a narrow dock.
                None => pane()
                    .h_full()
                    .w(gpui::Rems(props::SIDEBAR_REMS))
                    .max_w(relative(0.45))
                    .flex_none(),
            };
            (first, pane().h_full().flex_1())
        };
        let container = if p.vertical { ui::v_flex() } else { ui::h_flex().items_stretch() };
        Ok(container
            .flex_1()
            .size_full()
            .min_h_0()
            .child(first.child(a))
            .child(Self::build_divider(&Props { vertical: !p.vertical, ..Props::default() }))
            .child(second.child(b))
            .into_any_element())
    }

    /// `empty{ "message", action }`: centered muted text for empty states.
    pub(super) fn build_empty(
        &mut self,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let text = Self::text_of(t, start)?.or_else(|| p.label.clone()).unwrap_or_default();
        let label = Self::muted_label(text, p, cx)?;
        Ok(ui::v_flex()
            .flex_1()
            .size_full()
            .items_center()
            .justify_center()
            .gap(DynamicSpacing::Base08.rems(cx))
            .p(DynamicSpacing::Base16.rems(cx))
            .child(div().max_w_full().text_center().child(label))
            .children(self.slot(t, "action", window, cx)?)
            .into_any_element())
    }

    /// `input{ id, placeholder, multiline }`: a single- or multi-line editor.
    pub(super) fn build_input(
        &mut self,
        p: &Props,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let id = p.id.clone().ok_or_else(|| rt_err("needs an `id`"))?;
        let multiline = p.multiline;
        let placeholder = p.placeholder.clone();
        let editor = self
            .editors
            .entry(id.clone())
            .or_insert_with(|| {
                cx.new(|cx| {
                    let mut e = if multiline {
                        let mut e = Editor::multi_line(window, cx);
                        // A text box, not a code editor: no line numbers.
                        e.set_show_gutter(false, cx);
                        e
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
        let el = Self::style(ui::v_flex().w_full().min_w_0(), p, cx)?;
        Ok(el.child(editor).into_any_element())
    }

    /// `col`/`row`: plain flex containers.
    pub(super) fn build_flex(
        &mut self,
        tag: &str,
        t: &Table,
        p: &Props,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> mlua::Result<AnyElement> {
        let el = if tag == "row" { ui::h_flex() } else { ui::v_flex() };
        let mut el = Self::style(el.min_w_0(), p, cx)?;
        if tag == "row" && p.wrap {
            el = el.flex_wrap();
        }
        let el = el.children(self.children(t, start, window, cx)?);

        let on_click = Self::callback(t, "on_click")?;
        let scroll = p.scroll.unwrap_or(false);
        if p.id.is_none() && on_click.is_none() && !scroll {
            return Ok(el.into_any_element());
        }
        let id = self.id_or_auto(p, tag);
        if scroll {
            if on_click.is_some() {
                return Err(rt_err("`scroll` and `on_click` can't be combined"));
            }
            return Ok(self.scrollable(el.min_h_0(), id, window, cx));
        }
        let mut el = el.id(SharedString::from(id));
        if let Some(f) = on_click {
            el = el.cursor_pointer().on_click(self.click(f, cx));
        }
        Ok(el.into_any_element())
    }

    /// Layout and look props shared by containers.
    fn style(mut el: Div, p: &Props, cx: &gpui::App) -> mlua::Result<Div> {
        if let Some(v) = &p.p {
            el = el.p(props::spacing(v, "p", cx)?);
        }
        if let Some(v) = &p.px {
            el = el.px(props::spacing(v, "px", cx)?);
        }
        if let Some(v) = &p.py {
            el = el.py(props::spacing(v, "py", cx)?);
        }
        if let Some(v) = &p.gap {
            el = el.gap(props::spacing(v, "gap", cx)?);
        }
        match &p.rounded {
            None | Some(props::Toggle::Bool(false)) => {}
            Some(props::Toggle::Num(n)) => {
                props::deprecated(format!("rounded = {n} (px) is deprecated: use rounded = true"));
                el = el.rounded(gpui::px(*n));
            }
            Some(_) => el = el.rounded_sm(),
        }
        if let Some(size) = &p.size {
            el = el.text_size(match props::label_size(size)? {
                LabelSize::Default => ui::TextSize::Default.rems(cx),
                LabelSize::Small => ui::TextSize::Small.rems(cx),
                LabelSize::XSmall => ui::TextSize::XSmall.rems(cx),
                LabelSize::Large => ui::TextSize::Large.rems(cx),
                _ => ui::TextSize::Default.rems(cx),
            });
        }
        match &p.w {
            None => {}
            Some(l) => match props::rems_len(l, "w")? {
                None => el = el.w_full(),
                Some(len) => el = el.w(len).flex_none(),
            },
        }
        match &p.h {
            None => {}
            Some(Len::Name(n)) if n == "full" => el = el.h_full(),
            Some(l) => {
                if let Some(len) = props::rems_len(l, "h")? {
                    el = el.h(len).flex_none();
                }
            }
        }
        if p.grow {
            el = el.flex_1().min_h_0().min_w_0();
        }
        if let Some(c) = &p.bg {
            el = el.bg(props::hsla(c, cx)?);
        }
        if let Some(c) = &p.color {
            el = el.text_color(props::hsla(c, cx)?);
        }
        match &p.border {
            None | Some(props::Toggle::Bool(false)) => {}
            Some(props::Toggle::Str(c)) => el = el.border_1().border_color(props::hsla(c, cx)?),
            Some(_) => el = el.border_1().border_color(cx.theme().colors().border_variant),
        }
        if p.mono {
            el = el.font_family(".ZedMono");
        }
        if p.bold {
            el = el.font_weight(gpui::FontWeight::SEMIBOLD);
        }
        match p.align.as_deref() {
            None => {}
            Some("center") => el = el.items_center(),
            Some("end") => el = el.items_end(),
            Some("start") => el = el.items_start(),
            Some("stretch") => el = el.items_stretch(),
            Some(other) => return Err(rt_err(format!("bad align {other:?}"))),
        }
        match p.justify.as_deref() {
            None => {}
            Some("center") => el = el.justify_center(),
            Some("end") => el = el.justify_end(),
            Some("between") => el = el.justify_between(),
            Some("start") => el = el.justify_start(),
            Some(other) => return Err(rt_err(format!("bad justify {other:?}"))),
        }
        Ok(el)
    }
}
