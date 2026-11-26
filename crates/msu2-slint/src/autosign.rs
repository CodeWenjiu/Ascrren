//! 自动签到的纯调度逻辑，时间由调用方传入，便于测试。
//!
//! 签到窗口来自 `course_signin::schedule::Timeline`（上课前 1~5 分钟随机
//! 开启，在下课前都有效）。本模块只负责：窗口开启时给出待签课程、失败后
//! 60 秒重试、成功后不再重复、下课后放弃。

use std::collections::{BTreeMap, HashSet};

use chrono::NaiveDateTime;
use course_signin::model::Course;
use course_signin::schedule::{CourseTimeline, SIGN_RETRY_INTERVAL, Timeline};

/// 触发一次签到尝试的原因，用于日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// 签到窗口刚开启（含程序在窗口内启动）。
    WindowOpen,
    /// 已到上课时刻，最后一次确认。
    ClassBegin,
    /// 窗口内失败后的重试。
    Retry,
}

/// 一次签到尝试的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 上游确认已签到。
    Success,
    /// 失败，窗口内稍后重试。
    Retry,
}

/// 当日课表的签到状态机。
pub(crate) struct Scheduler {
    timeline: Timeline,
    /// 时间线索引 → 重试时刻。
    retry_at: BTreeMap<usize, NaiveDateTime>,
    /// 已确认签到的课程 id。
    signed: HashSet<String>,
    /// 正在签到的课程 id，结果回来前不再触发。
    in_flight: HashSet<String>,
}

impl Scheduler {
    pub(crate) fn new() -> Self {
        Self {
            timeline: Timeline::default(),
            retry_at: BTreeMap::new(),
            signed: HashSet::new(),
            in_flight: HashSet::new(),
        }
    }

    /// 课表刷新成功：重建时间线。本地已确认的成功会保留，避免上游
    /// 课表还没反映时重复签到。
    pub(crate) fn on_refresh(&mut self, courses: &[Course]) {
        self.timeline = Timeline::from_courses(courses);
        self.retry_at.clear();
        self.in_flight.clear();

        let ids: HashSet<&str> = self
            .timeline
            .courses
            .iter()
            .map(|course| course.course_id.as_str())
            .collect();
        self.signed.retain(|id| ids.contains(id.as_str()));
        self.signed.extend(
            courses
                .iter()
                .filter(|course| course.sign_status == "1")
                .map(|course| course.id.clone()),
        );
    }

    /// 时间线第 `index` 门课。
    pub(crate) fn course(&self, index: usize) -> Option<&CourseTimeline> {
        self.timeline.courses.get(index)
    }

    /// 签到请求已发出。
    pub(crate) fn mark_started(&mut self, course_id: &str) {
        self.in_flight.insert(course_id.to_owned());
    }

    /// 一次签到尝试结束。
    pub(crate) fn on_result(&mut self, course_id: &str, outcome: Outcome, now: NaiveDateTime) {
        self.in_flight.remove(course_id);
        match outcome {
            Outcome::Success => {
                self.signed.insert(course_id.to_owned());
                if let Some(index) = self.timeline.index_of(course_id) {
                    self.retry_at.remove(&index);
                }
            }
            Outcome::Retry => {
                if let Some(index) = self.timeline.index_of(course_id) {
                    let still_open = self
                        .timeline
                        .courses
                        .get(index)
                        .is_some_and(|course| now <= course.class_end);
                    if still_open {
                        self.retry_at.insert(index, now + SIGN_RETRY_INTERVAL);
                    }
                }
            }
        }
    }

    /// 到点的签到动作：`(时间线索引, 触发原因)`。
    pub(crate) fn take_due(&mut self, now: NaiveDateTime) -> Option<(usize, Trigger)> {
        for (index, course) in self.timeline.courses.iter().enumerate() {
            if now > course.class_end {
                self.retry_at.remove(&index);
                continue;
            }
            if self.signed.contains(&course.course_id) || self.in_flight.contains(&course.course_id)
            {
                self.retry_at.remove(&index);
                continue;
            }
            if let Some(at) = self.retry_at.get(&index) {
                if *at <= now {
                    return Some((index, Trigger::Retry));
                }
                continue;
            }
            if course.window_contains(now) {
                let trigger = if now >= course.class_begin {
                    Trigger::ClassBegin
                } else {
                    Trigger::WindowOpen
                };
                return Some((index, trigger));
            }
        }
        None
    }

    /// 下一个需要唤醒的时刻；已在窗口内时返回 `now`。
    pub(crate) fn next_deadline(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        let mut deadline: Option<NaiveDateTime> = None;
        for (index, course) in self.timeline.courses.iter().enumerate() {
            if now > course.class_end
                || self.signed.contains(&course.course_id)
                || self.in_flight.contains(&course.course_id)
            {
                continue;
            }

            let at = match self.retry_at.get(&index) {
                Some(at) => *at,
                None if now >= course.window_open => return Some(now),
                None => course.window_open,
            };
            deadline = Some(deadline.map_or(at, |current| current.min(at)));
        }
        deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(time: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M:%S").expect("time")
    }

    fn course(id: &str, open: &str, begin: &str, end: &str) -> CourseTimeline {
        CourseTimeline {
            course_id: id.to_owned(),
            course_name: format!("课{id}"),
            window_open: at(open),
            class_begin: at(begin),
            class_end: at(end),
        }
    }

    fn scheduler(courses: Vec<CourseTimeline>) -> Scheduler {
        let mut scheduler = Scheduler::new();
        scheduler.timeline = Timeline { courses };
        scheduler
    }

    fn upstream_course(id: &str, status: &str) -> Course {
        Course {
            id: id.to_owned(),
            uuid: id.to_owned(),
            course_name: format!("课{id}"),
            teacher_name: String::new(),
            week_day: "1".to_owned(),
            class_begin_time: "2026-03-25 10:00:00".to_owned(),
            class_end_time: "2026-03-25 11:40:00".to_owned(),
            sign_status: status.to_owned(),
        }
    }

    #[test]
    fn signs_when_the_window_opens() {
        let mut scheduler = scheduler(vec![course(
            "a",
            "2026-03-25 09:58:00",
            "2026-03-25 10:00:00",
            "2026-03-25 11:40:00",
        )]);

        assert_eq!(scheduler.take_due(at("2026-03-25 09:57:00")), None);
        assert_eq!(
            scheduler.take_due(at("2026-03-25 09:58:00")),
            Some((0, Trigger::WindowOpen))
        );
    }

    #[test]
    fn retries_one_minute_after_a_failure() {
        let mut scheduler = scheduler(vec![course(
            "a",
            "2026-03-25 09:58:00",
            "2026-03-25 10:00:00",
            "2026-03-25 11:40:00",
        )]);
        let now = at("2026-03-25 09:58:00");

        assert_eq!(scheduler.take_due(now), Some((0, Trigger::WindowOpen)));
        scheduler.mark_started("a");
        assert_eq!(scheduler.take_due(now), None);

        scheduler.on_result("a", Outcome::Retry, now);
        assert_eq!(scheduler.take_due(at("2026-03-25 09:58:59")), None);
        assert_eq!(
            scheduler.take_due(at("2026-03-25 09:59:00")),
            Some((0, Trigger::Retry))
        );
    }

    #[test]
    fn success_stops_further_attempts() {
        let mut scheduler = scheduler(vec![course(
            "a",
            "2026-03-25 09:58:00",
            "2026-03-25 10:00:00",
            "2026-03-25 11:40:00",
        )]);
        scheduler.mark_started("a");
        scheduler.on_result("a", Outcome::Success, at("2026-03-25 09:58:00"));

        assert_eq!(scheduler.take_due(at("2026-03-25 10:30:00")), None);
        assert_eq!(scheduler.next_deadline(at("2026-03-25 09:58:00")), None);
    }

    #[test]
    fn gives_up_after_the_class_ends() {
        let mut scheduler = scheduler(vec![course(
            "a",
            "2026-03-25 09:58:00",
            "2026-03-25 10:00:00",
            "2026-03-25 11:40:00",
        )]);
        assert_eq!(scheduler.take_due(at("2026-03-25 11:41:00")), None);
    }

    #[test]
    fn uses_the_class_begin_trigger_inside_the_window() {
        let mut scheduler = scheduler(vec![course(
            "a",
            "2026-03-25 09:58:00",
            "2026-03-25 10:00:00",
            "2026-03-25 11:40:00",
        )]);
        assert_eq!(
            scheduler.take_due(at("2026-03-25 10:05:00")),
            Some((0, Trigger::ClassBegin))
        );
    }

    #[test]
    fn skips_courses_signed_upstream() {
        let mut scheduler = Scheduler::new();
        scheduler.on_refresh(&[upstream_course("a", "1")]);
        assert_eq!(scheduler.take_due(at("2026-03-25 10:05:00")), None);
    }

    #[test]
    fn keeps_local_success_when_upstream_lags() {
        let mut scheduler = Scheduler::new();
        scheduler.on_refresh(&[upstream_course("a", "0")]);
        scheduler.on_result("a", Outcome::Success, at("2026-03-25 09:58:00"));

        // 上游课表仍然显示未签，但本地不应重复签。
        scheduler.on_refresh(&[upstream_course("a", "0")]);
        assert_eq!(scheduler.take_due(at("2026-03-25 10:05:00")), None);
    }

    #[test]
    fn next_deadline_points_at_the_window() {
        let scheduler = scheduler(vec![course(
            "a",
            "2026-03-25 09:58:00",
            "2026-03-25 10:00:00",
            "2026-03-25 11:40:00",
        )]);
        assert_eq!(
            scheduler.next_deadline(at("2026-03-25 09:00:00")),
            Some(at("2026-03-25 09:58:00"))
        );
        // 进入窗口后立刻唤醒。
        assert_eq!(
            scheduler.next_deadline(at("2026-03-25 09:59:00")),
            Some(at("2026-03-25 09:59:00"))
        );
    }

    #[test]
    fn timeline_drops_stale_signed_ids() {
        let mut scheduler = Scheduler::new();
        scheduler.on_refresh(&[upstream_course("a", "1")]);
        // 第二天的课表换了 id，旧的已签记录不应保留。
        scheduler.on_refresh(&[upstream_course("b", "0")]);
        assert!(!scheduler.signed.contains("a"));
    }
}
