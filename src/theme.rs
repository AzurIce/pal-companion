//! 主题系统：浅色 / 深色 / 跟随系统三态，持久化 + matchMedia 监听。

use crate::storage::use_persistent;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub mode: Signal<ThemeMode>,
    /// 当前生效是否为深色
    #[allow(dead_code)] // 对外备用（内部 data-theme 已生效）
    pub is_dark: Memo<bool>,
}

/// 在 App 根部调用一次。
pub fn use_theme() -> Theme {
    let mode = use_persistent("pal-companion:theme", || ThemeMode::System);
    let mut system_dark = use_signal(|| false);

    // 安装系统主题监听（仅一次，监听生命周期与页面一致）
    use_effect(move || {
        let Some(window) = web_sys::window() else { return };
        let Ok(Some(mql)) = window.match_media("(prefers-color-scheme: dark)") else {
            return;
        };
        system_dark.set(mql.matches());
        let mql_in_closure = mql.clone();
        let cb = Closure::wrap(Box::new(move || {
            system_dark.set(mql_in_closure.matches());
        }) as Box<dyn FnMut()>);
        mql.set_onchange(Some(cb.as_ref().unchecked_ref()));
        cb.forget();
    });

    let is_dark = use_memo(move || match mode() {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => system_dark(),
    });

    // 生效主题写入 <html data-theme>
    use_effect(move || {
        let theme = if is_dark() { "dark" } else { "light" };
        if let Some(el) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let _ = el.set_attribute("data-theme", theme);
        }
    });

    Theme { mode, is_dark }
}
