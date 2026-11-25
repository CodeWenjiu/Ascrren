//! Slint 窗口适配器、页面状态和统计。

use std::rc::Rc;
use std::time::{Duration, Instant};

use msu2_core::ButtonState;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::{Platform, WindowAdapter};

use crate::{AppWindow, PAGES};

pub(crate) struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

/// 安装无窗口的 Slint 平台。
pub(crate) fn install_platform(window: Rc<MinimalSoftwareWindow>) {
    slint::platform::set_platform(Box::new(HeadlessPlatform {
        window,
        start: Instant::now(),
    }))
    .expect("Slint platform was already initialized");
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) rendered: u64,
    pub(crate) sent: u64,
    pub(crate) dirty_tiles: u64,
    pub(crate) rects: u64,
    pub(crate) bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppState {
    pub(crate) page: usize,
}

impl AppState {
    pub(crate) fn new() -> Self {
        Self { page: 0 }
    }

    pub(crate) fn advance(&mut self) {
        self.page = (self.page + 1) % PAGES;
    }
}

pub(crate) fn refresh_ui(ui: &AppWindow, state: &AppState, button: &ButtonState) {
    ui.set_page(state.page as i32);
    ui.set_pressed(button.is_pressed());
}
