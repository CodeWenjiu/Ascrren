//! 单键电容触摸输入：去抖、滤波与单击识别。
//!
//! 设备背面只有一个接到 ADC9 的覆铜焊盘，固件不推送事件，主机只能轮询
//! `read_adc(9)`。本模块把原始采样变成「一次完整的按下-释放」，并处理
//! 三个现实问题：
//!
//! - **基线漂移**：环境电容随温湿度和手的位置变化；
//! - **抖动**：ADC 噪声会让一次按压在阈值附近反复翻转；
//! - **回弹**：松手后焊盘的值会在阈值上方振荡，容易连续触发。
//!
//! 按下时 ADC 值会**下降**（厂商 Demo 用 `baseline - 125` 判定按下）。
//!
//! 这颗焊盘的时间响应不足以可靠区分长按和短按，因此这里只识别单击。

use std::time::{Duration, Instant};

/// 一次完整的按下-释放。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// 从首次检测到按下到确认释放的时长，仅供参考。
    pub held: Duration,
}

/// 按键识别的参数。
#[derive(Debug, Clone, Copy)]
pub struct ButtonConfig {
    /// 相对基线下降多少判为按下。
    pub press_delta: u16,
    /// 相对基线下降多少仍算按住；必须小于 `press_delta` 才形成迟滞。
    pub release_delta: u16,
    /// 校准所需的采样数。
    pub calibration_samples: u8,
    /// 确认按下所需的连续采样数。
    pub debounce_samples: u8,
    /// 确认释放所需的连续采样数。
    pub release_debounce_samples: u8,
    /// 采样中值滤波窗口；1 表示关闭。
    ///
    /// 窗口越大越能抑制单点噪声，但会多延迟约半个窗口的采样。
    pub median_window: u8,
    /// 释放后多久内忽略新的按下。
    ///
    /// 电容焊盘松手后会在阈值上方反弹，不加锁定会连续触发点击。
    pub release_lockout: Duration,
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
            calibration_samples: 8,
            debounce_samples: 2,
            release_debounce_samples: 2,
            median_window: 3,
            release_lockout: Duration::from_millis(250),
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
    Pressed { since: Instant },
}

/// 单键单击状态机。
pub struct ButtonState {
    config: ButtonConfig,
    phase: Phase,
    calibration: Vec<u16>,
    calibration_min: u16,
    calibration_max: u16,
    baseline: u32,
    history: Vec<u16>,
    last_filtered: u16,
    candidate: Option<bool>,
    candidate_samples: u8,
    candidate_since: Option<Instant>,
    last_release: Option<Instant>,
}

impl ButtonState {
    pub fn new(config: ButtonConfig) -> Self {
        Self {
            config,
            phase: Phase::Calibrating,
            calibration: Vec::with_capacity(usize::from(config.calibration_samples)),
            calibration_min: 0,
            calibration_max: 0,
            baseline: 0,
            history: Vec::with_capacity(usize::from(config.median_window).max(1)),
            last_filtered: 0,
            candidate: None,
            candidate_samples: 0,
            candidate_since: None,
            last_release: None,
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

    /// 输入一次采样，在释放时返回这次按压。
    pub fn sample(&mut self, raw: u16, now: Instant) -> Option<Press> {
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
        let locked = self.last_release.is_some_and(|released| {
            now.saturating_duration_since(released) < self.config.release_lockout
        });
        let raw_pressed = if locked {
            false
        } else {
            match self.phase {
                Phase::Pressed { .. } => u32::from(raw) < self.config.release_threshold(baseline),
                _ => u32::from(raw) < self.config.press_threshold(baseline),
            }
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
                    // 从第一次疑似按下的采样开始计时，便于报告近似按住时长。
                    self.phase = Phase::Pressed {
                        since: self.candidate_since.unwrap_or(now),
                    };
                    self.candidate_samples = 0;
                }
                (Phase::Pressed { since }, false) => {
                    self.phase = Phase::Idle;
                    self.candidate_samples = 0;
                    self.last_release = Some(now);
                    return Some(Press {
                        held: now.saturating_duration_since(since),
                    });
                }
                _ => {}
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
    fn press_reports_release_once() {
        let (mut state, start) = calibrated();

        assert_eq!(state.sample(800, start), None);
        assert_eq!(state.sample(800, at(start, 10)), None);
        assert!(state.is_pressed());

        assert_eq!(state.sample(1000, at(start, 100)), None);
        let press = state.sample(1000, at(start, 110)).expect("release");
        assert_eq!(press.held, Duration::from_millis(110));
        assert!(!state.is_pressed());

        // 继续停在松开状态不会重复触发。
        assert_eq!(state.sample(1000, at(start, 400)), None);
        assert_eq!(state.sample(1000, at(start, 410)), None);
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
    fn release_lockout_swallows_bounce_back() {
        let (mut state, start) = calibrated();

        state.sample(800, start);
        state.sample(800, at(start, 10));
        state.sample(1000, at(start, 100));
        assert!(state.sample(1000, at(start, 110)).is_some());

        // 松手后值回弹到阈值以下，但还在 250 ms 锁定期内。
        for offset in [140, 150, 160, 170] {
            assert_eq!(state.sample(800, at(start, offset)), None);
        }
        assert!(!state.is_pressed());
    }

    #[test]
    fn median_filter_rejects_a_single_spike() {
        let (mut state, start) = calibrated_with(ButtonConfig {
            median_window: 3,
            ..config()
        });

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
    fn reports_calibration_spread() {
        let (state, _) = calibrated();
        assert_eq!(state.calibration_spread(), 1100 - 980);
    }
}
