//! 课程签到：登录、拉取课表、窗口内自动签到。
//!
//! 网络请求跑在独立任务里，通过 channel 把快照推给渲染循环，避免阻塞帧输出。
//! 刷新与签到共用一条时间线：睡到下一个窗口/重试/刷新时刻才醒来。

use std::time::Duration;

use chrono::{Local, NaiveDateTime, TimeDelta};
use course_signin::model::{AuthSession, Course, SignResult};
use course_signin::schedule::{next_hourly, sign_timestamp};
use course_signin::upstream::Client;
use tokio::sync::mpsc;

use crate::autosign::{Outcome, Scheduler};

/// 课表刷新失败后的重试间隔。
const REFRESH_RETRY: TimeDelta = TimeDelta::minutes(5);

/// 最近一次拉取的结果快照。
#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub(crate) courses: Vec<Course>,
    /// 最近一次刷新失败的原因；成功后清空。
    pub(crate) error: Option<String>,
    /// 最近一次成功刷新的时刻（`HH:MM`）。
    pub(crate) updated: Option<String>,
    /// 最近一次签到事件，例如 `已签 高等数学`。
    pub(crate) event: Option<String>,
}

/// 后台任务的配置。
pub(crate) struct Config {
    pub(crate) user: String,
    pub(crate) password: String,
    pub(crate) auto_sign: bool,
}

/// 启动后台任务：首次立即刷新，之后每个整点刷新；开启自动签到后按
/// 窗口触发签到。
pub(crate) fn spawn(config: Config, tx: mpsc::Sender<Snapshot>) {
    tokio::spawn(async move {
        let client = match Client::with_credentials_default_base(config.user, config.password) {
            Ok(client) => client,
            Err(error) => {
                let _ = tx
                    .send(Snapshot {
                        error: Some(format!("配置无效：{error}")),
                        ..Snapshot::default()
                    })
                    .await;
                return;
            }
        };

        let mut session: Option<AuthSession> = None;
        let mut scheduler = Scheduler::new();
        let mut courses: Vec<Course> = Vec::new();
        let mut error: Option<String> = None;
        let mut updated: Option<String> = None;
        let mut event: Option<String> = None;
        let mut last_date: Option<String> = None;
        let mut next_refresh = Local::now().naive_local();

        loop {
            let now = Local::now().naive_local();

            if now >= next_refresh {
                let date = now.format("%Y%m%d").to_string();
                match fetch_schedule(&client, &mut session, &date).await {
                    Ok(fetched) => {
                        println!("schedule: {} courses", fetched.len());
                        // 跨天后旧的签到事件不再展示。
                        if last_date.as_deref() != Some(date.as_str()) {
                            event = None;
                        }
                        last_date = Some(date);
                        courses = fetched;
                        scheduler.on_refresh(&courses);
                        updated = Some(now.format("%H:%M").to_string());
                        error = None;
                        next_refresh = next_hourly(now);
                    }
                    Err(message) => {
                        eprintln!("schedule refresh failed: {message}");
                        error = Some(message);
                        next_refresh = now + REFRESH_RETRY;
                    }
                }
                if tx
                    .send(snapshot(&courses, &error, &updated, &event))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }

            if config.auto_sign
                && let Some((index, trigger)) = scheduler.take_due(now)
            {
                let Some(course) = scheduler.course(index) else {
                    continue;
                };
                let course_id = course.course_id.clone();
                let name = course.course_name.clone();

                println!("sign attempt: {name} ({trigger:?})");
                scheduler.mark_started(&course_id);
                event = Some(format!("{name} 签到中"));
                if tx
                    .send(snapshot(&courses, &error, &updated, &event))
                    .await
                    .is_err()
                {
                    return;
                }

                let outcome = match sign_course(&client, &course_id).await {
                    Ok(result) if result.success || result.message.contains("已签到") => {
                        println!("sign ok: {name} ({})", result.message);
                        if let Some(course) =
                            courses.iter_mut().find(|course| course.id == course_id)
                        {
                            course.sign_status = "1".to_owned();
                        }
                        event = Some(format!("已签 {name}"));
                        Outcome::Success
                    }
                    Ok(result) => {
                        eprintln!("sign failed: {name}: {}", result.message);
                        event = Some(format!("{name} 重试中"));
                        Outcome::Retry
                    }
                    Err(message) => {
                        eprintln!("sign failed: {name}: {message}");
                        event = Some(format!("{name} 重试中"));
                        Outcome::Retry
                    }
                };

                scheduler.on_result(&course_id, outcome, Local::now().naive_local());
                if tx
                    .send(snapshot(&courses, &error, &updated, &event))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }

            // 睡到下一个签到窗口、重试时刻或刷新时刻。
            let wake = if config.auto_sign {
                scheduler
                    .next_deadline(now)
                    .map_or(next_refresh, |deadline| deadline.min(next_refresh))
            } else {
                next_refresh
            };
            let millis = (wake - now).num_milliseconds().clamp(200, 3_600_000);
            tokio::time::sleep(Duration::from_millis(millis as u64)).await;
        }
    });
}

fn snapshot(
    courses: &[Course],
    error: &Option<String>,
    updated: &Option<String>,
    event: &Option<String>,
) -> Snapshot {
    Snapshot {
        courses: courses.to_vec(),
        error: error.clone(),
        updated: updated.clone(),
        event: event.clone(),
    }
}

/// 登录（如需要）并拉取某天课表。
///
/// 课表请求失败时丢弃缓存的会话：常见原因是会话过期，下次调用会重新登录。
async fn fetch_schedule(
    client: &Client,
    session: &mut Option<AuthSession>,
    date: &str,
) -> Result<Vec<Course>, String> {
    if session.is_none() {
        *session = Some(client.login().await.map_err(describe)?);
    }

    let current = session.as_ref().expect("session was just set");
    match client.get_schedule(current, date).await {
        Ok(courses) => Ok(courses),
        Err(error) => {
            *session = None;
            Err(describe(error))
        }
    }
}

/// 登录 → 校准服务器时间 → 直接签到。
async fn sign_course(client: &Client, course_id: &str) -> Result<SignResult, String> {
    let session = client.login().await.map_err(describe)?;
    let server_time = client.server_timestamp().await.map_err(describe)?;
    client
        .scan_sign(&session, course_id, sign_timestamp(server_time))
        .await
        .map_err(describe)
}

/// 带上完整 cause 链的错误文本，方便在终端排查。
fn describe(error: impl std::error::Error + Send + Sync + 'static) -> String {
    format!("{:#}", anyhow::Error::new(error))
}

/// 今日尚未结束的课程，按开始时间排序。
pub(crate) fn upcoming(courses: &[Course], now: NaiveDateTime) -> Vec<&Course> {
    let mut list: Vec<&Course> = courses
        .iter()
        .filter(|course| parse_time(&course.class_end_time).is_some_and(|end| end >= now))
        .collect();
    list.sort_by_key(|course| parse_time(&course.class_begin_time));
    list
}

/// 下一节未结束的课程及其开始时刻。
pub(crate) fn next_course(
    courses: &[Course],
    now: NaiveDateTime,
) -> Option<(&Course, NaiveDateTime)> {
    upcoming(courses, now)
        .into_iter()
        .filter_map(|course| parse_time(&course.class_begin_time).map(|begin| (course, begin)))
        .find(|(_, begin)| *begin >= now)
}

/// 单门课程的一行摘要，例如 `10:25 高等数学 待签`。
pub(crate) fn course_line(course: &Course) -> String {
    let time = time_only(&course.class_begin_time);
    let status = match course.sign_status.as_str() {
        "1" => "已签",
        "0" => "待签",
        _ => "?",
    };
    format!("{time} {} {status}", truncate(&course.course_name, 5))
}

fn parse_time(raw: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S").ok()
}

fn time_only(datetime: &str) -> String {
    datetime
        .rsplit(' ')
        .next()
        .unwrap_or(datetime)
        .chars()
        .take(5)
        .collect()
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_course_line() {
        let course = Course {
            id: "1".to_owned(),
            uuid: "u".to_owned(),
            course_name: "高等数学".to_owned(),
            teacher_name: "张".to_owned(),
            week_day: "1".to_owned(),
            class_begin_time: "2026-03-25 10:25:00".to_owned(),
            class_end_time: "2026-03-25 12:00:00".to_owned(),
            sign_status: "0".to_owned(),
        };
        assert_eq!(course_line(&course), "10:25 高等数学 待签");
    }

    #[test]
    fn truncates_long_names() {
        assert_eq!(truncate("超长课程名称示例", 5), "超长课程名");
    }
}
