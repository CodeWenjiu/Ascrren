//! Slint 窗口适配器、页面列表和 UI 刷新。

use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDateTime};
use msu2_core::ButtonState;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::{Platform, WindowAdapter};

use crate::AppWindow;
use crate::metrics::Metrics;
use crate::signin::{self, Snapshot};

/// 课程列表每页显示的课程数。
const COURSES_PER_PAGE: usize = 3;

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
    pub(crate) reconnects: u64,
}

/// 一个展示页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageKind {
    Overview,
    Signin,
    Courses(usize),
}

/// 当前页面列表。页数随课程数量变化。
pub(crate) struct AppState {
    page: usize,
    pages: Vec<PageKind>,
}

impl AppState {
    pub(crate) fn new() -> Self {
        Self {
            page: 0,
            pages: vec![PageKind::Overview],
        }
    }

    /// 重建页面列表。`course_count` 为 `None` 表示未启用签到。
    pub(crate) fn rebuild(&mut self, course_count: Option<usize>) {
        let mut pages = vec![PageKind::Overview];

        if let Some(count) = course_count {
            pages.push(PageKind::Signin);
            for index in 0..count.div_ceil(COURSES_PER_PAGE) {
                pages.push(PageKind::Courses(index));
            }
        }

        self.pages = pages;
        self.page = self.page.min(self.pages.len().saturating_sub(1));
    }

    pub(crate) fn advance(&mut self) {
        self.page = (self.page + 1) % self.pages.len().max(1);
    }

    pub(crate) fn current(&self) -> PageKind {
        self.pages
            .get(self.page)
            .copied()
            .unwrap_or(PageKind::Overview)
    }

    pub(crate) fn label(&self) -> String {
        format!("{}/{}", self.page + 1, self.pages.len().max(1))
    }
}

/// 每帧刷新：布局类型、按下高亮和页码。
pub(crate) fn refresh_page(ui: &AppWindow, state: &AppState, button: &ButtonState) {
    let kind = match state.current() {
        PageKind::Overview => 0,
        PageKind::Signin | PageKind::Courses(_) => 1,
    };

    ui.set_page_kind(kind);
    ui.set_pressed(button.is_pressed());
    ui.set_page_label(state.label().into());
}

/// 每秒刷新：时间、负载和速率。
pub(crate) fn refresh_metrics(ui: &AppWindow, metrics: &Metrics) {
    ui.set_clock_time(metrics.time.clone().into());
    ui.set_clock_date(metrics.date.clone().into());
    ui.set_cpu_percent(i32::from(metrics.cpu_percent));
    ui.set_cpu_text(format!("{}%", metrics.cpu_percent).into());
    ui.set_mem_percent(i32::from(metrics.mem_percent));
    ui.set_mem_text(format!("{}%", metrics.mem_percent).into());
    ui.set_net_down(metrics.net_down.clone().into());
    ui.set_net_up(metrics.net_up.clone().into());
}

/// 文本页内容：签到状态或课程列表。
pub(crate) fn refresh_text(ui: &AppWindow, state: &AppState, snapshot: Option<&Snapshot>) {
    let now = Local::now().naive_local();
    let (title, line1, line2, line3) = match state.current() {
        PageKind::Signin => signin_lines(snapshot, now),
        PageKind::Courses(index) => course_lines(snapshot, index, now),
        _ => return,
    };

    ui.set_text_title(title.into());
    ui.set_text_line1(line1.into());
    ui.set_text_line2(line2.into());
    ui.set_text_line3(line3.into());
}

fn signin_lines(
    snapshot: Option<&Snapshot>,
    now: NaiveDateTime,
) -> (String, String, String, String) {
    let title = String::from("SIGNIN");

    let Some(snapshot) = snapshot else {
        return (
            title,
            String::from("等待登录"),
            String::new(),
            String::new(),
        );
    };

    // 从来没有拉到过课表时，直接把错误显示出来。
    if snapshot.courses.is_empty()
        && let Some(error) = &snapshot.error
    {
        return (title, short(error, 12), String::new(), String::new());
    }

    let remaining = signin::upcoming(&snapshot.courses, now).len();
    let line1 = format!("今日 {remaining} 节课");

    let line2 = match signin::next_course(&snapshot.courses, now) {
        Some((course, begin)) => format!(
            "下节 {} {}",
            begin.format("%H:%M"),
            short(&course.course_name, 4)
        ),
        None => String::from("今日无课"),
    };

    let line3 = if let Some(event) = &snapshot.event {
        event.clone()
    } else if snapshot.error.is_some() {
        String::from("课表刷新失败")
    } else {
        snapshot
            .updated
            .as_ref()
            .map(|time| format!("更新 {time}"))
            .unwrap_or_default()
    };

    (title, line1, line2, line3)
}

fn course_lines(
    snapshot: Option<&Snapshot>,
    index: usize,
    now: NaiveDateTime,
) -> (String, String, String, String) {
    let Some(snapshot) = snapshot else {
        return (
            String::from("COURSE"),
            String::from("无数据"),
            String::new(),
            String::new(),
        );
    };

    let upcoming = signin::upcoming(&snapshot.courses, now);
    let pages = upcoming.len().div_ceil(COURSES_PER_PAGE).max(1);
    let title = format!("COURSE {}/{}", index + 1, pages);

    let mut lines = [String::new(), String::new(), String::new()];
    for (slot, course) in lines.iter_mut().zip(
        upcoming
            .iter()
            .copied()
            .skip(index * COURSES_PER_PAGE)
            .take(COURSES_PER_PAGE),
    ) {
        *slot = signin::course_line(course);
    }

    // 空位用刷新时间填上，避免尾页大片留白。
    if lines[2].is_empty() {
        lines[2] = snapshot
            .updated
            .as_ref()
            .map(|updated| format!("更新 {updated}"))
            .unwrap_or_default();
    }

    (title, lines[0].clone(), lines[1].clone(), lines[2].clone())
}

fn short(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use course_signin::model::Course;

    fn course(name: &str, begin: &str, end: &str, status: &str) -> Course {
        Course {
            id: name.to_owned(),
            uuid: name.to_owned(),
            course_name: name.to_owned(),
            teacher_name: String::new(),
            week_day: "1".to_owned(),
            class_begin_time: begin.to_owned(),
            class_end_time: end.to_owned(),
            sign_status: status.to_owned(),
        }
    }

    #[test]
    fn pages_grow_with_course_count() {
        let mut state = AppState::new();
        state.rebuild(None);
        assert_eq!(state.label(), "1/1");

        state.rebuild(Some(0));
        assert_eq!(state.label(), "1/2");

        state.rebuild(Some(7));
        // 概览 + 签到 + ceil(7/3) = 3 个课程页
        assert_eq!(state.label(), "1/5");
    }

    #[test]
    fn page_index_is_clamped_after_shrinking() {
        let mut state = AppState::new();
        state.rebuild(Some(9));
        for _ in 0..4 {
            state.advance();
        }
        assert_eq!(state.label(), "5/5");

        state.rebuild(Some(0));
        assert_eq!(state.label(), "2/2");
    }

    #[test]
    fn signin_lines_report_an_error() {
        let snapshot = Snapshot {
            error: Some("登录失败：超时".to_owned()),
            ..Snapshot::default()
        };
        let now = NaiveDateTime::parse_from_str("2026-03-25 09:00:00", "%Y-%m-%d %H:%M:%S")
            .expect("time");
        let (title, line1, _, _) = signin_lines(Some(&snapshot), now);
        assert_eq!(title, "SIGNIN");
        assert!(line1.contains("登录失败"));
    }

    #[test]
    fn signin_lines_show_the_latest_event() {
        let snapshot = Snapshot {
            courses: vec![course(
                "高等数学",
                "2026-03-25 10:25:00",
                "2026-03-25 12:00:00",
                "1",
            )],
            updated: Some("09:01".to_owned()),
            event: Some("已签 高等数学".to_owned()),
            ..Snapshot::default()
        };
        let now = NaiveDateTime::parse_from_str("2026-03-25 10:30:00", "%Y-%m-%d %H:%M:%S")
            .expect("time");
        let (_, _, _, line3) = signin_lines(Some(&snapshot), now);
        assert_eq!(line3, "已签 高等数学");
    }

    #[test]
    fn keeps_showing_stale_courses_when_a_refresh_fails() {
        let snapshot = Snapshot {
            courses: vec![course(
                "高等数学",
                "2026-03-25 10:25:00",
                "2026-03-25 12:00:00",
                "0",
            )],
            error: Some("课表请求失败".to_owned()),
            updated: Some("09:01".to_owned()),
            ..Snapshot::default()
        };
        let now = NaiveDateTime::parse_from_str("2026-03-25 09:00:00", "%Y-%m-%d %H:%M:%S")
            .expect("time");
        let (_, line1, _, line3) = signin_lines(Some(&snapshot), now);
        assert_eq!(line1, "今日 1 节课");
        assert_eq!(line3, "课表刷新失败");
    }

    #[test]
    fn course_page_skips_finished_courses() {
        let snapshot = Snapshot {
            courses: vec![
                course("早课", "2026-03-25 08:00:00", "2026-03-25 09:00:00", "1"),
                course("午课", "2026-03-25 10:00:00", "2026-03-25 12:00:00", "0"),
            ],
            ..Snapshot::default()
        };
        let now = NaiveDateTime::parse_from_str("2026-03-25 09:30:00", "%Y-%m-%d %H:%M:%S")
            .expect("time");

        let (title, line1, line2, line3) = course_lines(Some(&snapshot), 0, now);
        assert_eq!(title, "COURSE 1/1");
        assert_eq!(line1, "10:00 午课 待签");
        assert!(line2.is_empty());
        assert!(line3.is_empty());
    }
}
