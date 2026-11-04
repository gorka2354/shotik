//! Windows implementation of the platform ports.
#![cfg(windows)]

pub mod displays;
pub mod dip_layout;
pub mod capture;
pub mod window_source;
pub mod clipboard;
pub mod theme;
pub mod shell;
pub mod dialogs;
pub mod autostart;
pub mod ocr;
pub mod selection;
pub mod hotkeys;
pub mod tray;
pub mod single_instance;
