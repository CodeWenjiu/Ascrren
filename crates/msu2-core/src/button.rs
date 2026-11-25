//! 单键电容触摸输入：去抖、动态基线与手势识别。
//!
//! 设备背面只有一个接到 ADC9 的覆铜焊盘，固件不推送事件，主机只能轮询
//! `read_adc(9)`。本模块把原始采样变成可用的手势，并处理两个现实问题：
//!
//! - **基线漂移**：环境电容随温湿度和手的位置变化，固定阈值会失效；
//! - **抖动**：ADC 噪声会让一次按压在阈值附近反复翻转。
//!
//! 按下时 ADC 值会**下降**（厂商 Demo 用 `baseline - 125` 判定按下）。

use std::time::{Duration, Instant};

/// 识别出的手势。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// 短按后释放。
    Click,
    /// 按住达到 [`ButtonConfig::long_press`]，在按住期间触发一次。
    LongPress,
    /// 按住达到 [`ButtonConfig::very_long_press`]，在按住期间触发一次。
    VeryLongPress,
}

/// 手势识别的参数。
#[derive(Debug, Clone, Copy)]
pub struct ButtonConfig {
    /// 相对基线下降多少判为按下。
    pub press_delta: u16,
    /// 相对基线下降多少仍算按住；必须小于 `press_delta` 才形成迟滞。
    pub release_delta: u16,
    /// 触发长按的按住时长。
    pub long_press: Duration,
    /// 触发超长按的按住时长；应当大于 `long_press`。
    pub very_long_press: Duration,
    /// 校准所需的采样数。
    pub calibration_samples: u8,
    /// 确认按下所需的连续采样数。
    pub debounce_samples: u8,
    /// 确认释放所需的连续采样数。
    ///
    /// 比按下更小可以降低延迟，但太小会让焊盘回弹时的抖动反复触发点击。
    pub release_debounce_samples: u8,
    /// 采样中值滤波窗口；1 表示关闭。
    ///
    /// 窗口越大越能抑制单点噪声，但会多延迟约半个窗口的采样。
    pub median_window: u8,
    /// 是否在空闲时跟踪基线漂移。
    ///
    /// 默认关闭——ADC 噪声会让 EMA 随机游走，反而使阈值漂移。
    pub track_baseline: bool,
    /// 基线跟踪的 EMA 位移；越大跟踪越慢。
    pub baseline_ema_shift: u8,
}

impl Default for ButtonConfig {
    fn default() -> Self {
        Self {
            press_delta: 125,
            release_delta: 80,
            long_press: Duration::from_millis(600),
            very_long_press: Duration::from_millis(1500),
            calibration_samples: 8,
            debounce_samples: 2,
            release_debounce_samples: 2,
            median_window: 3,
            track_baseline: false,
            baseline_ema_shift: 6,
        }
    }
}

impl ButtonConfig {
    fn press_threshold(&self, baseline: u32) -> u32 {
        baseline.saturating_sub(u32::from(self.press_delta))
    }

    fn release_threshold(&self, baseline: u32) -> u32 {
        baseline.saturating_sub(u32::from(self.release_delta))
    }
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    Calibrating,
    Idle,
    Pressed {
        since: Instant,
        long_fired: bool,
        very_long_fired: bool,
    },
}

/// 单键手势状态机。
///
/// 每次采样调用 [`ButtonState::sample`]；状态翻转和长按都会在采样点触发，
/// 因此采样频率决定了手势的时间分辨率。
pub struct ButtonState {
    config: ButtonConfig,
    phase: Phase,
    calibration: Vec<u16>,
    baseline: u32,
    calibration_min: u16,
    calibration_max: u16,
    history: Vec<u16>,
    last_filtered: u16,
    candidate: Option<bool>,
    candidate_samples: u8,
    candidate_since: Option<Instant>,
}

impl ButtonState {
    pub fn new(config: ButtonConfig) -> Self {
        Self {
            config,
            phase: Phase::Calibrating,
            calibration: Vec::with_capacity(usize::from(config.calibration_samples)),
            baseline: 0,
            calibration_min: 0,
            calibration_max: 0,
            history: Vec::with_capacity(usize::from(config.median_window).max(1)),
            last_filtered: 0,
            candidate: None,
            candidate_samples: 0,
            candidate_since: None,
        }
    }

    /// 是否已完成校准。
    pub fn is_calibrated(&self) -> bool {
        !matches!(self.phase, Phase::Calibrating)
    }

    /// 校准得到的基线值。
    pub fn baseline(&self) -> u32 {
        self.baseline
    }

    /// 校准样本的极差；很大说明校准时手在动，结果不可信。
    pub fn calibration_spread(&self) -> u16 {
        self.calibration_max.saturating_sub(self.calibration_min)
    }

    /// 最近一次采样经过中值滤波后的值。
    pub fn last_filtered(&self) -> u16 {
        self.last_filtered
    }

    /// 当前是否处于按下状态。
    pub fn is_pressed(&self) -> bool {
        matches!(self.phase, Phase::Pressed { .. })
    }

    /// 按住进度，0.0 到 1.0，用于界面反馈。
    pub fn hold_progress(&self, now: Instant) -> f32 {
        match self.phase {
            Phase::Pressed { since, .. } => {
                let held = now.saturating_duration_since(since).as_secs_f32();
                (held / self.config.long_press.as_secs_f32()).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    /// 输入一次采样，返回本次采样触发的手势。
    pub fn sample(&mut self, raw: u16, now: Instant) -> Option<Gesture> {
        if matches!(self.phase, Phase::Calibrating) {
            self.calibration.push(raw);
            if self.calibration.len() >= usize::from(self.config.calibration_samples.max(1)) {
                self.calibration.sort_unstable();
                self.calibration_min = self.calibration[0];
                self.calibration_max = self.calibration[self.calibration.len() - 1];
                self.baseline = u32::from(self.calibration[self.calibration.len() / 2]);
                self.phase = Phase::Idle;
            }
            return None;
        }

        let raw = self.filter(raw);
        self.last_filtered = raw;

        // 空闲且开启跟踪时缓慢修正温漂；按下期间冻结基线，避免把按压
        // 本身当成漂移吸收掉。默认关闭，因为 ADC 噪声会让 EMA 随机游走。
        if self.config.track_baseline && matches!(self.phase, Phase::Idle) {
            let target = u32::from(raw);
            let window = u32::from(self.config.press_delta / 4);
            let diff = target.abs_diff(self.baseline);
            if diff > 0 && diff <= window {
                let shift = u32::from(self.config.baseline_ema_shift.clamp(1, 16));
                // 整数除法会把小偏差量化成 0，因此保证每次至少移动 1。
                let step = (diff >> shift).max(1).min(diff);
                if target > self.baseline {
                    self.baseline += step;
                } else {
                    self.baseline -= step;
                }
            }
        }

        let baseline = self.baseline;
        let raw_pressed = match self.phase {
            Phase::Pressed { .. } => u32::from(raw) < self.config.release_threshold(baseline),
            _ => u32::from(raw) < self.config.press_threshold(baseline),
        };

        if self.candidate == Some(raw_pressed) {
            self.candidate_samples = self.candidate_samples.saturating_add(1);
        } else {
            self.candidate = Some(raw_pressed);
            self.candidate_samples = 1;
            self.candidate_since = Some(now);
        }

        let needed = if raw_pressed {
            self.config.debounce_samples
        } else {
            self.config.release_debounce_samples
        }
        .max(1);

        if self.candidate_samples >= needed {
            match (self.phase, raw_pressed) {
                (Phase::Idle, true) => {
                    // 从第一次疑似按下的采样开始计时，避免去抖延迟把
                    // 一次轻点算成长按。
                    self.phase = Phase::Pressed {
                        since: self.candidate_since.unwrap_or(now),
                        long_fired: false,
                        very_long_fired: false,
                    };
                    self.candidate_samples = 0;
                }
                (Phase::Pressed { long_fired, .. }, false) => {
                    self.phase = Phase::Idle;
                    self.candidate_samples = 0;
                    if !long_fired {
                        return Some(Gesture::Click);
                    }
                }
                _ => {}
            }
        }

        if let Phase::Pressed {
            since,
            long_fired,
            very_long_fired,
        } = &mut self.phase
        {
            let held = now.saturating_duration_since(*since);
            if !*very_long_fired && held >= self.config.very_long_press {
                *very_long_fired = true;
                *long_fired = true;
                return Some(Gesture::VeryLongPress);
            }
            if !*long_fired && held >= self.config.long_press {
                *long_fired = true;
                return Some(Gesture::LongPress);
            }
        }

        None
    }

    fn filter(&mut self, raw: u16) -> u16 {
        let window = usize::from(self.config.median_window.clamp(1, 9));
        if window <= 1 {
            return raw;
        }
        self.history.push(raw);
        while self.history.len() > window {
            self.history.remove(0);
        }
        let mut sorted = self.history.clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ButtonConfig {
        ButtonConfig {
            calibration_samples: 4,
            debounce_samples: 2,
            release_debounce_samples: 2,
            median_window: 1,
            long_press: Duration::from_millis(300),
            very_long_press: Duration::from_millis(900),
            ..ButtonConfig::default()
        }
    }

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn calibrated_with(config: ButtonConfig) -> (ButtonState, Instant) {
        let mut state = ButtonState::new(config);
        let start = Instant::now();
        for sample in [1000, 1020, 980, 1100] {
            assert_eq!(state.sample(sample, start), None);
        }
        assert!(state.is_calibrated());
        (state, at(start, 10))
    }

    fn calibrated() -> (ButtonState, Instant) {
        calibrated_with(config())
    }

    #[test]
    fn calibrates_to_the_median_baseline() {
        let (state, _) = calibrated();
        // 排序后为 [980, 1000, 1020, 1100]，中间值是 1020。
        assert_eq!(state.baseline(), 1020);
    }

    #[test]
    fn click_on_short_press() {
        let (mut state, start) = calibrated();

        assert_eq!(state.sample(800, start), None);
        assert_eq!(state.sample(800, at(start, 10)), None);
        assert!(state.is_pressed());

        // 释放需要两次采样确认。
        assert_eq!(state.sample(1000, at(start, 100)), None);
        assert_eq!(state.sample(1000, at(start, 110)), Some(Gesture::Click));
        assert!(!state.is_pressed());
    }

    #[test]
    fn long_press_fires_while_held() {
        let (mut state, start) = calibrated();
        state.sample(800, start);
        state.sample(800, at(start, 10));

        assert_eq!(state.sample(800, at(start, 200)), None);
        assert_eq!(state.sample(800, at(start, 310)), Some(Gesture::LongPress));
        assert_eq!(state.sample(800, at(start, 400)), None);

        // 长按后的释放不再产生 Click。
        assert_eq!(state.sample(1000, at(start, 500)), None);
    }

    #[test]
    fn hold_time_starts_at_the_first_press_sample() {
        let (mut state, start) = calibrated();
        // 首次疑似按下就应当开始计时，而不是等去抖确认之后。
        state.sample(800, start);
        state.sample(800, at(start, 20));

        // 从 start 算起已经 310 ms，达到长按阈值。
        assert_eq!(state.sample(800, at(start, 310)), Some(Gesture::LongPress));
    }

    #[test]
    fn very_long_press_skips_long_press() {
        let (mut state, start) = calibrated();
        state.sample(800, start);
        state.sample(800, at(start, 10));

        assert_eq!(
            state.sample(800, at(start, 1000)),
            Some(Gesture::VeryLongPress)
        );
        assert_eq!(state.sample(800, at(start, 1100)), None);
    }

    #[test]
    fn single_noise_sample_does_not_press() {
        let (mut state, start) = calibrated();

        assert_eq!(state.sample(800, start), None);
        assert!(!state.is_pressed());

        assert_eq!(state.sample(1000, at(start, 10)), None);
        assert!(!state.is_pressed());
    }

    #[test]
    fn hysteresis_holds_until_release_threshold() {
        let (mut state, start) = calibrated();
        state.sample(800, start);
        state.sample(800, at(start, 10));
        assert!(state.is_pressed());

        // 900 高于按下阈值 895，但低于释放阈值 940，仍然算按住。
        state.sample(900, at(start, 100));
        state.sample(900, at(start, 110));
        assert!(state.is_pressed());

        state.sample(960, at(start, 200));
        state.sample(960, at(start, 210));
        assert!(!state.is_pressed());
    }

    #[test]
    fn median_filter_rejects_a_single_spike() {
        let (mut state, start) = calibrated_with(ButtonConfig {
            median_window: 3,
            ..config()
        });

        // 基线附近的一次尖峰不应触发按下。
        state.sample(1000, at(start, 0));
        state.sample(700, at(start, 10));
        state.sample(1000, at(start, 20));
        assert!(!state.is_pressed());
    }

    #[test]
    fn median_filter_passes_a_sustained_press() {
        let (mut state, start) = calibrated_with(ButtonConfig {
            median_window: 3,
            ..config()
        });

        state.sample(1000, at(start, 0));
        state.sample(700, at(start, 10));
        state.sample(700, at(start, 20));
        assert_eq!(state.last_filtered(), 700);
        state.sample(700, at(start, 30));
        assert!(state.is_pressed());
    }

    #[test]
    fn reports_calibration_spread() {
        let (state, _) = calibrated();
        assert_eq!(state.calibration_spread(), 1100 - 980);
    }

    #[test]
    fn does_not_track_baseline_by_default() {
        let (mut state, start) = calibrated();
        let before = state.baseline();

        for index in 0..64u64 {
            state.sample(1030, at(start, index * 10));
        }

        assert_eq!(state.baseline(), before);
    }

    #[test]
    fn tracks_slow_baseline_drift_when_enabled() {
        let (mut state, start) = calibrated_with(ButtonConfig {
            track_baseline: true,
            ..config()
        });
        let before = state.baseline();

        for index in 0..64u64 {
            state.sample(1030, at(start, index * 10));
        }

        assert!(state.baseline() > before);
        assert!(state.baseline() <= 1030);
    }

    #[test]
    fn hold_progress_reaches_one_at_long_press() {
        let (mut state, start) = calibrated();
        state.sample(800, start);
        state.sample(800, at(start, 10));

        assert!(state.hold_progress(at(start, 10)) < 0.1);
        assert!((state.hold_progress(at(start, 160)) - 0.5).abs() < 0.05);
        assert_eq!(state.hold_progress(at(start, 400)), 1.0);
    }
}
