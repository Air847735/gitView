//! gitview 桌面應用程式的內部模組。
//!
//! 以 library 形式公開，讓整合測試能直接驗證介面所消費的資料層 ——
//! 繪製本身無法自動驗證，但送進畫面的資料可以。

pub mod commands;
pub mod dto;
pub mod service;
pub mod settings;
pub mod watcher;
