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
    /// 释放后需要连续多少次「安静」采样才重新允许按下。
    ///
    /// 比时间锁定更可靠：即使电平长时间停在低位，也不会重复触发。
    pub rearm_samples: u8,
    /// 判定为安静的电平幅度：值恢复到 `baseline - rearm_delta` 以上。
    pub rearm_delta: u16,
    /// 长时间无法重新武装时强制放行，避免电平卡在中间导致按键彻底失效。
    pub rearm_timeout: Duration,
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
            press_delta: 150,
            release_delta: 100,
            calibration_samples: 8,
            debounce_samples: 2,
            release_debounce_samples: 2,
            median_window: 5,
            release_lockout: Duration::from_millis(800),
            rearm_samples: 5,
            rearm_delta: 30,
            rearm_timeout: Duration::from_secs(5),
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
    baseline: u32,
    history: Vec<u16>,
    candidate: Option<bool>,
    candidate_samples: u8,
    candidate_since: Option<Instant>,
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
            history: Vec::with_capacity(usize::from(config.median_window).max(1)),
            candidate: None,
            candidate_samples: 0,
            candidate_since: None,
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
        matches!(self.phase, Phase::Pressed { .. })
    }

    /// 是否需要更密的采样。
    ///
    /// 按住中或等待电平恢复时，状态可能在两次采样之间变化，需要高频
    /// 盯紧；完全空闲时可以降频，减少 USB 往返。
    pub fn needs_fast_sampling(&self) -> bool {
        !self.armed || matches!(self.phase, Phase::Pressed { .. })
    }

    /// 输入一次采样，在释放时返回这次按压。
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

        let raw = self.filter(raw);

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

        // 重新武装：必须先看到电平回到安静区间并保持若干次，才接受
        // 下一次按下。这样即使松手后电平停在低位，也不会重复触发。
        if !self.armed {
            let timed_out = self.last_release.is_some_and(|released| {
                now.saturating_duration_since(released) >= self.config.rearm_timeout
            });
            if timed_out {
                self.armed = true;
                self.rearm_quiet = 0;
            } else {
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

        let locked = self.last_release.is_some_and(|released| {
            now.saturating_duration_since(released) < self.config.release_lockout
        });
        let raw_pressed = if !self.armed || locked {
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
                    self.armed = false;
                    self.rearm_quiet = 0;
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
mod tests;
