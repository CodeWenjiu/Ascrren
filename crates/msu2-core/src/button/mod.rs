//! 单键电容触摸输入。
//!
//! 设备背面只有一个接到 ADC9 的覆铜焊盘，固件不推送事件，主机只能轮询
//! `read_adc(9)`。本模块把它变成「一次按下」事件，处理三件事：
//!
//! - **误码**：USB 偶尔把 ADC 读成 0 或 4095，偏离基线过大的采样直接丢弃；
//! - **噪声**：强信号单次触发，弱信号要求连续两次一致；
//! - **回弹**：松手后电平可能停在低位，必须恢复到安静区间才允许下一次按下。
//!
//! 按下时 ADC 值会**下降**（厂商 Demo 用 `baseline - 125` 判定按下）。
//!
//! 这颗焊盘的时间响应不足以可靠区分长按和短按，因此这里只识别单击。

use std::time::{Duration, Instant};

/// 一次按下。在检测到按下的那一刻就返回，不需要等松手。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// 触发按下的那次采样值，用于诊断异常读数。
    pub trigger_raw: u16,
}

/// 按键识别的参数。
#[derive(Debug, Clone, Copy)]
pub struct ButtonConfig {
    /// 相对基线下降多少判为按下。
    pub press_delta: u16,
    /// 相对基线下降多少仍算按住；必须小于 `press_delta` 才形成迟滞。
    pub release_delta: u16,
    /// 下降幅度达到这个值时，单次采样就确认按下。
    ///
    /// 强信号不需要去抖，只有刚过阈值的弱信号才需要多次采样确认。
    pub strong_press_delta: u16,
    /// 校准所需的采样数。
    pub calibration_samples: u8,
    /// 确认按下或释放所需的连续采样数。
    pub debounce_samples: u8,
    /// 释放后需要连续多少次「安静」采样才重新允许按下。
    pub rearm_samples: u8,
    /// 判定为安静的电平幅度：值恢复到 `baseline - rearm_delta` 以上。
    pub rearm_delta: u16,
    /// 长时间无法重新武装时强制放行，避免电平卡在中间导致按键彻底失效。
    pub rearm_timeout: Duration,
    /// 偏离基线超过这个幅度就视为无效采样。
    pub max_deviation: u16,
}

impl Default for ButtonConfig {
    fn default() -> Self {
        Self {
            press_delta: 150,
            release_delta: 100,
            strong_press_delta: 300,
            calibration_samples: 8,
            debounce_samples: 2,
            rearm_samples: 3,
            rearm_delta: 30,
            rearm_timeout: Duration::from_secs(5),
            max_deviation: 2000,
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
    Pressed,
}

/// 单键单击状态机。
pub struct ButtonState {
    config: ButtonConfig,
    phase: Phase,
    calibration: Vec<u16>,
    baseline: u32,
    candidate: Option<bool>,
    candidate_samples: u8,
    last_release: Option<Instant>,
    armed: bool,
    rearm_quiet: u8,
}

impl ButtonState {
    pub fn new(config: ButtonConfig) -> Self {
        Self {
            config,
            phase: Phase::Calibrating,
            calibration: Vec::with_capacity(usize::from(config.calibration_samples)),
            baseline: 0,
            candidate: None,
            candidate_samples: 0,
            last_release: None,
            armed: true,
            rearm_quiet: 0,
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

    /// 当前是否处于按下状态。
    pub fn is_pressed(&self) -> bool {
        matches!(self.phase, Phase::Pressed)
    }

    /// 是否需要更密的采样。
    ///
    /// 按住中或等待电平恢复时，状态可能在两次采样之间变化，需要高频
    /// 盯紧；完全空闲时可以降频，减少 USB 往返。
    pub fn needs_fast_sampling(&self) -> bool {
        !self.armed || matches!(self.phase, Phase::Pressed)
    }

    /// 输入一次采样，在检测到按下的那一刻返回事件。
    pub fn sample(&mut self, raw: u16, now: Instant) -> Option<Press> {
        if matches!(self.phase, Phase::Calibrating) {
            self.calibration.push(raw);
            if self.calibration.len() >= usize::from(self.config.calibration_samples.max(1)) {
                self.calibration.sort_unstable();
                self.baseline = u32::from(self.calibration[self.calibration.len() / 2]);
                self.phase = Phase::Idle;
            }
            return None;
        }

        let baseline = self.baseline;

        // USB 误码偶尔会把 ADC 读成 0 或 4095，这类值不可能是真实按压。
        if (i32::from(raw) - baseline as i32).unsigned_abs() > u32::from(self.config.max_deviation)
        {
            return None;
        }

        if !self.armed {
            self.try_rearm(raw, baseline, now);
        }

        let raw_pressed = if self.armed {
            match self.phase {
                Phase::Pressed => u32::from(raw) < self.config.release_threshold(baseline),
                _ => u32::from(raw) < self.config.press_threshold(baseline),
            }
        } else {
            false
        };

        if self.candidate == Some(raw_pressed) {
            self.candidate_samples = self.candidate_samples.saturating_add(1);
        } else {
            self.candidate = Some(raw_pressed);
            self.candidate_samples = 1;
        }

        let strong = raw_pressed
            && baseline.saturating_sub(u32::from(raw)) >= u32::from(self.config.strong_press_delta);
        let needed = if strong {
            1
        } else {
            self.config.debounce_samples.max(1)
        };

        if self.candidate_samples >= needed {
            match (self.phase, raw_pressed) {
                (Phase::Idle, true) => {
                    // 按下沿立即上报。按住期间状态停在 Pressed，因此不会
                    // 因为电平抖动而重复触发。
                    self.phase = Phase::Pressed;
                    self.candidate_samples = 0;
                    return Some(Press { trigger_raw: raw });
                }
                (Phase::Pressed, false) => {
                    self.phase = Phase::Idle;
                    self.candidate_samples = 0;
                    self.last_release = Some(now);
                    self.armed = false;
                    self.rearm_quiet = 0;
                }
                _ => {}
            }
        }

        None
    }

    /// 松手后必须看到电平回到安静区间并保持若干次，才接受下一次按下。
    fn try_rearm(&mut self, raw: u16, baseline: u32, now: Instant) {
        let timed_out = self.last_release.is_some_and(|released| {
            now.saturating_duration_since(released) >= self.config.rearm_timeout
        });
        if timed_out {
            self.armed = true;
            self.rearm_quiet = 0;
            return;
        }

        let quiet_level = baseline.saturating_sub(u32::from(self.config.rearm_delta));
        if u32::from(raw) >= quiet_level {
            self.rearm_quiet = self.rearm_quiet.saturating_add(1);
            if self.rearm_quiet >= self.config.rearm_samples.max(1) {
                self.armed = true;
                self.rearm_quiet = 0;
            }
        } else {
            self.rearm_quiet = 0;
        }
    }
}

#[cfg(test)]
mod tests;
