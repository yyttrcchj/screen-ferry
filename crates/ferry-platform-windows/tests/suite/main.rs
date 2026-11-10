//! ferry-platform-windows 的测试入口。
//!
//! 测试按 `src/` 的模块结构在本目录下镜像组织，见 AGENTS.md 第 9 节。
//! 只在 Windows 上编译运行（CI 的 windows 任务）。

#![cfg(target_os = "windows")]

mod clipboard;
mod elevation;
mod keymap;
mod update;
