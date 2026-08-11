//! shadcn 风格 UI 套件：Button / Badge / Combobox / Dialog / Segmented / ThemeToggle。

use crate::theme::{Theme, ThemeMode};
use dioxus::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};
use wasm_bindgen::JsCast;

static NEXT_COMBOBOX_ID: AtomicU64 = AtomicU64::new(1);

/// document 级 mousedown 监听，卸载时自动移除。
struct DocumentClickListener {
    target: web_sys::EventTarget,
    callback: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
}

impl DocumentClickListener {
    fn install(on_outside: impl FnMut(web_sys::Event) + 'static) -> Option<Self> {
        let document = web_sys::window()?.document()?;
        let target: web_sys::EventTarget = document.into();
        let callback = wasm_bindgen::closure::Closure::wrap(Box::new(on_outside)
            as Box<dyn FnMut(web_sys::Event)>);
        target
            .add_event_listener_with_callback("mousedown", callback.as_ref().unchecked_ref())
            .ok()?;
        Some(Self { target, callback })
    }
}

impl Drop for DocumentClickListener {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(
            "mousedown",
            self.callback.as_ref().unchecked_ref(),
        );
    }
}

// ---------- Button ----------

#[derive(Clone, Copy, PartialEq, Default)]
#[allow(dead_code)] // UI 套件保留完整变体集
pub enum BtnVariant {
    #[default]
    Primary,
    Outline,
    Ghost,
    Destructive,
}

#[component]
pub fn Button(
    #[props(default)] variant: BtnVariant,
    #[props(default)] sm: bool,
    #[props(default)] icon: bool,
    #[props(default)] disabled: bool,
    #[props(default)] class: String,
    #[props(default)] onclick: EventHandler<MouseEvent>,
    children: Element,
) -> Element {
    let variant_class = match variant {
        BtnVariant::Primary => "btn--primary",
        BtnVariant::Outline => "btn--outline",
        BtnVariant::Ghost => "btn--ghost",
        BtnVariant::Destructive => "btn--destructive",
    };
    let size = match (sm, icon) {
        (true, true) => "btn--sm btn--icon",
        (false, true) => "btn--icon",
        (true, false) => "btn--sm",
        (false, false) => "",
    };
    rsx! {
        button {
            class: "btn {variant_class} {size} {class}",
            disabled,
            onclick: move |e| onclick.call(e),
            {children}
        }
    }
}

// ---------- Badge ----------

#[derive(Clone, Copy, PartialEq, Default)]
#[allow(dead_code)] // UI 套件保留完整变体集
pub enum BadgeKind {
    #[default]
    Default,
    Gold,
    Violet,
    Negative,
    Owned,
}

impl BadgeKind {
    fn class(self) -> &'static str {
        match self {
            BadgeKind::Default => "",
            BadgeKind::Gold => "badge--gold",
            BadgeKind::Violet => "badge--violet",
            BadgeKind::Negative => "badge--negative",
            BadgeKind::Owned => "badge--owned",
        }
    }
}

#[component]
pub fn Badge(#[props(default)] kind: BadgeKind, children: Element) -> Element {
    rsx! {
        span { class: "badge {kind.class()}", {children} }
    }
}

// ---------- Combobox ----------

#[derive(Clone, PartialEq)]
pub struct ComboOption {
    /// 内部值（internal_name）
    pub value: String,
    /// 主标签（中文名）
    pub label: String,
    /// 次标签（英文名等）
    pub sublabel: Option<String>,
    /// 图标 URL
    pub icon: Option<String>,
    /// 效果描述（选项第二行小字）
    pub desc: Option<String>,
    /// 稀有度徽标（文字 + 配色）
    pub badge: Option<(String, BadgeKind)>,
}

impl ComboOption {
    #[allow(dead_code)] // 便捷构造器，备用
    pub fn simple(value: String, label: String, sublabel: Option<String>, icon: Option<String>) -> Self {
        Self {
            value,
            label,
            sublabel,
            icon,
            desc: None,
            badge: None,
        }
    }
}

/// 可搜索下拉选择器。直接读写父组件传入的 signal。
#[component]
pub fn Combobox(
    options: Vec<ComboOption>,
    value: Signal<Option<String>>,
    #[props(default = "搜索…".to_string())] placeholder: String,
) -> Element {
    let mut query = use_signal(String::new);
    let mut open = use_signal(|| false);
    let mut highlighted = use_signal(|| 0usize);
    // 注意：输入框是非受控的（不绑 value），否则任何重渲染都会把旧值写回 DOM、
    // 打断中文输入法的组合过程。选中后输入框被"已选卡片"替换，无需程序回写。

    // 点击组件外部时收起下拉（每个实例独立标识，Drop 时移除监听）
    let instance_id = use_hook(|| NEXT_COMBOBOX_ID.fetch_add(1, Ordering::Relaxed));
    use_hook(move || {
        let attr = format!("[data-combobox=\"{instance_id}\"]");
        DocumentClickListener::install(move |event| {
            if !*open.peek() {
                return;
            }
            let inside = event
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .and_then(|el| el.closest(&attr).ok().flatten())
                .is_some();
            if !inside {
                open.set(false);
            }
        })
        .map(std::rc::Rc::new)
    });

    let q = query.read().to_lowercase();
    let matches: Vec<ComboOption> = options
        .iter()
        .filter(|o| {
            q.is_empty()
                || o.label.to_lowercase().contains(&q)
                || o.sublabel.as_deref().unwrap_or("").to_lowercase().contains(&q)
                || o.value.to_lowercase().contains(&q)
        })
        .take(60)
        .cloned()
        .collect();
    // 注意：不在渲染期写 signal（会导致重渲染死循环）。
    // highlighted 的不变量由事件处维护：oninput 归零、keydown 取模、mouseenter 设为有效索引。

    let selected = value.read().clone();
    let selected_opt = selected
        .as_ref()
        .and_then(|v| options.iter().find(|o| &o.value == v).cloned());

    rsx! {
        div { class: "combobox", "data-combobox": "{instance_id}",
            if let Some(opt) = selected_opt {
                div { class: "selected",
                    if let Some(icon) = &opt.icon {
                        img { src: icon.clone(), alt: "" }
                    }
                    span { class: "opt-label", "{opt.label}" }
                    if let Some(sub) = &opt.sublabel {
                        span { class: "opt-sub", "{sub}" }
                    }
                    button {
                        class: "clear",
                        onclick: move |_| value.set(None),
                        "×"
                    }
                }
            } else {
                input {
                    class: "input",
                    placeholder,
                    onfocus: move |_| open.set(true),
                    oninput: move |e| {
                        query.set(e.value());
                        open.set(true);
                        highlighted.set(0);
                    },
                    onkeydown: move |e| {
                        let n = matches.len();
                        if n == 0 { return; }
                        match e.key() {
                            Key::ArrowDown => {
                                e.prevent_default();
                                let next = (*highlighted.read() + 1) % n;
                                highlighted.set(next);
                            }
                            Key::ArrowUp => {
                                e.prevent_default();
                                let next = (*highlighted.read() + n - 1) % n;
                                highlighted.set(next);
                            }
                            Key::Enter => {
                                e.prevent_default();
                                if let Some(o) = matches.get(*highlighted.read()) {
                                    let v = o.value.clone();
                                    value.set(Some(v));
                                    query.set(String::new());
                                    open.set(false);
                                }
                            }
                            Key::Escape => open.set(false),
                            _ => {}
                        }
                    },
                }
                if *open.read() && !matches.is_empty() {
                    ul { class: "options",
                        for (i, o) in matches.iter().enumerate() {
                            li {
                                key: "{o.value}",
                                class: if i == *highlighted.read() { "highlighted" } else { "" },
                                // onmousedown 先于 input 失焦触发，保证点击生效
                                onmousedown: {
                                    let v = o.value.clone();
                                    move |_| {
                                        value.set(Some(v.clone()));
                                        query.set(String::new());
                                        open.set(false);
                                    }
                                },
                                onmouseenter: move |_| highlighted.set(i),
                                if let Some(icon) = &o.icon {
                                    img { src: icon.clone(), alt: "" }
                                }
                                div { class: "opt-main",
                                    div { class: "opt-line",
                                        span { class: "opt-label", "{o.label}" }
                                        if let Some((text, kind)) = &o.badge {
                                            Badge { kind: *kind, "{text}" }
                                        }
                                        if let Some(sub) = &o.sublabel {
                                            span { class: "opt-sub", "{sub}" }
                                        }
                                    }
                                    if let Some(desc) = &o.desc {
                                        div { class: "opt-desc", "{desc}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------- Dialog ----------

#[component]
pub fn Dialog(
    open: Signal<bool>,
    title: String,
    #[props(default)] description: Option<String>,
    children: Element,
) -> Element {
    if !*open.read() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "dialog-overlay",
            onclick: move |_| open.set(false),
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    open.set(false);
                }
            },
            div {
                class: "dialog",
                onclick: move |e| e.stop_propagation(),
                h2 { class: "dialog-title", "{title}" }
                if let Some(desc) = description {
                    p { class: "dialog-desc", "{desc}" }
                }
                {children}
            }
        }
    }
}

// ---------- Segmented ----------

#[derive(Clone, PartialEq)]
pub struct Segment {
    pub value: String,
    pub label: String,
}

#[component]
pub fn Segmented(options: Vec<Segment>, value: Signal<String>) -> Element {
    rsx! {
        div { class: "segmented",
            for seg in options {
                button {
                    key: "{seg.value}",
                    class: if *value.read() == seg.value { "active" } else { "" },
                    onclick: {
                        let v = seg.value.clone();
                        move |_| value.set(v.clone())
                    },
                    "{seg.label}"
                }
            }
        }
    }
}

// ---------- ThemeToggle ----------

#[component]
pub fn ThemeToggle() -> Element {
    let theme = use_context::<Theme>();
    let mut current = theme.mode;
    let items = [
        (ThemeMode::Light, "浅色"),
        (ThemeMode::Dark, "深色"),
        (ThemeMode::System, "系统"),
    ];
    rsx! {
        div { class: "segmented",
            for (m, label) in items {
                button {
                    key: "{label}",
                    class: if *current.read() == m { "active" } else { "" },
                    onclick: move |_| current.set(m),
                    "{label}"
                }
            }
        }
    }
}
