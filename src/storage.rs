//! localStorage 持久化 hook（官方文档推荐的 gloo-storage 模式）。

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// 挂载时从 localStorage 回读（失败则用 init），之后每次变更自动写回。
pub fn use_persistent<T>(key: &str, init: impl FnOnce() -> T) -> Signal<T>
where
    T: Serialize + DeserializeOwned + Clone + 'static,
{
    let key = key.to_string();
    let signal = use_signal(|| LocalStorage::get::<T>(&key).unwrap_or_else(|_| init()));
    use_effect(move || {
        let value = signal.read().clone();
        let _ = LocalStorage::set(&key, &value);
    });
    signal
}
