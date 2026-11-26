//! 课程签到：登录、拉取课表、计算签到窗口。
//!
//! 只负责读取和展示，不发起点名请求。网络请求跑在独立任务里，通过
//! channel 把快照推给渲染循环，避免阻塞帧输出。

use std::time::Duration;

use chrono::{Local, NaiveDateTime};
use course_signin::model::{AuthSession, Course};
use course_signin::upstream::Client;
use tokio::sync::mpsc;

/// 课表刷新间隔。
const REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// 最近一次拉取的结果快照。
#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub(crate) courses: Vec<Course>,
    pub(crate) error: Option<String>,
    pub(crate) updated: Option<String>,
}

/// 启动后台刷新任务。
pub(crate) fn spawn(user: String, password: String, tx: mpsc::Sender<Snapshot>) {
    tokio::spawn(async move {
        let client = match Client::with_credentials_default_base(user, password) {
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
        loop {
            let snapshot = refresh_once(&client, &mut session).await;
            if tx.send(snapshot).await.is_err() {
                return;
            }
            tokio::time::sleep(REFRESH_INTERVAL).await;
        }
    });
}

async fn refresh_once(client: &Client, session: &mut Option<AuthSession>) -> Snapshot {
    if session.is_none() {
        match client.login().await {
            Ok(fresh) => *session = Some(fresh),
            Err(error) => {
                return Snapshot {
                    error: Some(format!("登录失败：{error}")),
                    ..Snapshot::default()
                };
            }
        }
    }

    let today = Local::now().format("%Y%m%d").to_string();
    let Some(current) = session.as_ref() else {
        return Snapshot::default();
    };

    match client.get_schedule(current, &today).await {
        Ok(courses) => Snapshot {
            courses,
            error: None,
            updated: Some(Local::now().format("%H:%M").to_string()),
        },
        Err(error) => Snapshot {
            error: Some(format!("课表获取失败：{error}")),
            ..Snapshot::default()
        },
    }
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
