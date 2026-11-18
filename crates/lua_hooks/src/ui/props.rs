//! Typed element props: every string key of an element table is deserialized
//! into [`Props`] with `serde(deny_unknown_fields)`, so a misspelled prop is an
//! error shown in place of the element instead of being silently ignored.
//! Callbacks and element-valued slots aren't serde data; they are read
//! directly from the table (see [`CALLBACKS`] and [`SLOTS`]).

use gpui::{AbsoluteLength, App, Hsla, Rems, px, rgba};
use mlua::{Lua, LuaSerdeExt as _, Table, Value};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashSet;
use theme::ActiveTheme as _;
use ui::{Color, DynamicSpacing, IconSize, LabelSize};

pub(super) fn rt_err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

/// Function-valued props.
pub(super) const CALLBACKS: &[&str] = &["on_click", "on_select"];
/// Props holding elements (or element lists), built by the element itself.
pub(super) const SLOTS: &[&str] = &[
    "start", "end_", "end_hover", "action", "actions", "header", "footer",
];

#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct Props {
    pub id: Option<String>,
    pub label: Option<String>,
    pub title: Option<String>,
    pub context: Option<String>,
    pub detail: Option<String>,
    pub empty: Option<String>,
    pub placeholder: Option<String>,
    pub tooltip: Option<String>,
    pub icon: Option<String>,
    pub name: Option<String>,
    pub style: Option<String>,

    pub size: Option<Size>,
    #[serde(alias = "fg")]
    pub color: Option<String>,
    pub bg: Option<String>,
    pub border: Option<Toggle>,
    pub rounded: Option<Toggle>,

    pub p: Option<Space>,
    pub px: Option<Space>,
    pub py: Option<Space>,
    pub gap: Option<Space>,
    pub w: Option<Len>,
    pub h: Option<Len>,
    pub ratio: Option<f32>,
    pub align: Option<String>,
    pub justify: Option<String>,

    #[serde(default)]
    pub grow: bool,
    #[serde(default)]
    pub mono: bool,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub truncate: bool,
    pub scroll: Option<bool>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub vertical: bool,
    #[serde(default)]
    pub sub: bool,
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub wrap: bool,

    pub indent: Option<usize>,
    pub checked: Option<Toggle>,
    pub selected: Option<Toggle>,
    pub options: Option<Vec<String>>,
}

/// `true`/`false` or a string (`checked = "mixed"`, `selected = "debug"`,
/// `border = "accent"`, `rounded = true`).
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub(super) enum Toggle {
    Bool(bool),
    Num(f32),
    Str(String),
}

impl Toggle {
    pub fn is_true(&self) -> bool {
        match self {
            Toggle::Bool(b) => *b,
            Toggle::Num(_) | Toggle::Str(_) => true,
        }
    }
}

/// Text/icon size: a name, or (deprecated) a pixel number.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub(super) enum Size {
    Name(String),
    Px(f32),
}

/// Spacing: a token (`none xs sm md lg xl`, mapped to `DynamicSpacing` so it
/// follows UI density and font size), or (deprecated) a pixel number.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub(super) enum Space {
    Token(String),
    Px(f32),
}

/// Width/height: `"full"`, a size token (`"sidebar"`), or pixels.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub(super) enum Len {
    Name(String),
    Px(f32),
}

impl Props {
    /// Reads the string-keyed props of an element table.
    pub fn parse(lua: &Lua, t: &Table) -> mlua::Result<Props> {
        let data = lua.create_table()?;
        for pair in t.pairs::<Value, Value>() {
            let (k, v) = pair?;
            let Value::String(key) = &k else { continue };
            let key = key.to_str()?;
            if CALLBACKS.contains(&&*key) || SLOTS.contains(&&*key) {
                continue;
            }
            if matches!(v, Value::Function(_)) {
                return Err(rt_err(format!(
                    "unknown callback `{}` (expected one of {})",
                    &*key,
                    CALLBACKS.join(", ")
                )));
            }
            data.raw_set(k, v)?;
        }
        lua.from_value(Value::Table(data))
            .map_err(|e| rt_err(format!("bad props: {e}")))
    }
}

thread_local! {
    static WARNED: RefCell<HashSet<String>> = RefCell::default();
}

/// Logs a deprecation once per message instead of on every frame.
pub(super) fn deprecated(msg: String) {
    WARNED.with(|w| {
        if w.borrow_mut().insert(msg.clone()) {
            log::warn!(target: "lua_hooks", "{msg}");
        }
    });
}

pub(super) fn spacing(s: &Space, what: &str, cx: &App) -> mlua::Result<AbsoluteLength> {
    Ok(match s {
        Space::Token(t) => {
            let d = match t.as_str() {
                "none" => DynamicSpacing::Base00,
                "xs" => DynamicSpacing::Base02,
                "sm" => DynamicSpacing::Base04,
                "md" => DynamicSpacing::Base08,
                "lg" => DynamicSpacing::Base12,
                "xl" => DynamicSpacing::Base16,
                other => {
                    return Err(rt_err(format!(
                        "bad {what} {other:?}, expected none|xs|sm|md|lg|xl"
                    )));
                }
            };
            d.rems(cx).into()
        }
        Space::Px(n) => {
            deprecated(format!(
                "{what} = {n} (px) is deprecated: use none|xs|sm|md|lg|xl, which follow UI density"
            ));
            px(*n).into()
        }
    })
}

/// Width of a `"sidebar"` (rems, so it follows `ui_font_size`).
pub(super) const SIDEBAR_REMS: f32 = 16.;

pub(super) fn rems_len(l: &Len, what: &str) -> mlua::Result<Option<AbsoluteLength>> {
    Ok(match l {
        Len::Name(n) if n == "full" => None,
        Len::Name(n) if n == "sidebar" => Some(Rems(SIDEBAR_REMS).into()),
        Len::Name(n) => return Err(rt_err(format!("bad {what} {n:?}, expected full|sidebar|<px>"))),
        Len::Px(n) => Some(px(*n).into()),
    })
}

pub(super) fn label_size(s: &Size) -> mlua::Result<LabelSize> {
    Ok(match s {
        Size::Name(n) => match n.as_str() {
            "default" => LabelSize::Default,
            "small" => LabelSize::Small,
            "xsmall" => LabelSize::XSmall,
            "large" => LabelSize::Large,
            other => {
                return Err(rt_err(format!(
                    "bad size {other:?}, expected default|small|xsmall|large"
                )));
            }
        },
        Size::Px(n) => {
            deprecated(format!("size = {n} (px) is deprecated: use default|small|xsmall|large"));
            if *n <= 10. {
                LabelSize::XSmall
            } else if *n <= 12. {
                LabelSize::Small
            } else if *n <= 14. {
                LabelSize::Default
            } else {
                LabelSize::Large
            }
        }
    })
}

pub(super) fn icon_size(s: &Size) -> mlua::Result<IconSize> {
    Ok(match s {
        Size::Name(n) => match n.as_str() {
            "indicator" => IconSize::Indicator,
            "xsmall" => IconSize::XSmall,
            "small" => IconSize::Small,
            "default" | "medium" => IconSize::Medium,
            "large" | "xlarge" => IconSize::XLarge,
            other => return Err(rt_err(format!("bad icon size {other:?}"))),
        },
        Size::Px(n) => return Err(rt_err(format!("bad icon size {n}: use a name"))),
    })
}

/// Theme color by name (`background surface panel editor element hover text muted
/// accent border error warning success`) or `#rrggbb[aa]`.
pub(super) fn hsla(name: &str, cx: &App) -> mlua::Result<Hsla> {
    let c = cx.theme().colors();
    let s = cx.theme().status();
    Ok(match name {
        "background" => c.background,
        "surface" => c.surface_background,
        "panel" => c.panel_background,
        "editor" => c.editor_background,
        "element" => c.element_background,
        "hover" => c.element_hover,
        "text" | "default" => c.text,
        "muted" => c.text_muted,
        "accent" => c.text_accent,
        "border" => c.border,
        "border_variant" => c.border_variant,
        "error" => s.error,
        "warning" => s.warning,
        "success" => s.success,
        hex => {
            let bad = || rt_err(format!("bad color {name:?}"));
            let hex = hex.strip_prefix('#').ok_or_else(bad)?;
            let v = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
            match hex.len() {
                6 => rgba((v << 8) | 0xff).into(),
                8 => rgba(v).into(),
                _ => return Err(bad()),
            }
        }
    })
}

/// Named `ui::Color` (for labels and icons), or a custom theme/hex color.
pub(super) fn color(name: &str, cx: &App) -> mlua::Result<Color> {
    Ok(match name {
        "default" | "text" => Color::Default,
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
        _ => Color::Custom(hsla(name, cx)?),
    })
}

/// Parses a Lua icon name (`"settings"` or `"settings-off"`) into `ui::IconName`.
pub(super) fn icon_name(name: &str) -> mlua::Result<ui::IconName> {
    use std::str::FromStr as _;
    ui::IconName::from_str(&name.replace('-', "_"))
        .map_err(|_| rt_err(format!("unknown icon {name:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> mlua::Result<Props> {
        let lua = Lua::new();
        let t: Table = lua.load(src).eval()?;
        Props::parse(&lua, &t)
    }

    #[test]
    fn typed_props() {
        let p = parse(r#"{ "list_item", "name", selected = true, indent = 1, fg = "muted",
            gap = "sm", w = "sidebar", on_click = function() end, start = { "icon", name = "x" } }"#)
            .unwrap();
        assert!(p.selected.unwrap().is_true());
        assert_eq!(p.indent, Some(1));
        assert_eq!(p.color.as_deref(), Some("muted"));
        assert!(matches!(p.gap, Some(Space::Token(ref t)) if t == "sm"));
        assert!(matches!(parse(r#"{ "row", gap = 4 }"#).unwrap().gap, Some(Space::Px(_))));
        assert!(matches!(parse(r#"{ "checkbox", checked = "mixed" }"#).unwrap().checked,
            Some(Toggle::Str(_))));
    }

    #[test]
    fn rejects_typos() {
        let e = parse(r#"{ "row", pading = "sm" }"#).unwrap_err().to_string();
        assert!(e.contains("pading"), "{e}");
        let e = parse(r#"{ "button", onclick = function() end }"#).unwrap_err().to_string();
        assert!(e.contains("onclick"), "{e}");
    }
}
