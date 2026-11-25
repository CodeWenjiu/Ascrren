use super::*;

fn config() -> ButtonConfig {
    ButtonConfig {
        calibration_samples: 4,
        debounce_samples: 2,
        release_debounce_samples: 2,
        median_window: 1,
        release_lockout: Duration::from_millis(250),
        rearm_samples: 3,
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
fn stays_disarmed_while_the_level_remains_low() {
    let (mut state, start) = calibrated();

    state.sample(800, start);
    state.sample(800, at(start, 10));
    state.sample(1000, at(start, 20));
    assert!(state.sample(1000, at(start, 30)).is_some());

    // 电平一直停在低位：不应再触发任何按压。
    for index in 0..60u64 {
        assert_eq!(state.sample(700, at(start, 100 + index * 10)), None);
    }
    assert!(!state.is_pressed());
}

#[test]
fn rearms_after_the_level_recovers() {
    let (mut state, start) = calibrated();

    state.sample(800, start);
    state.sample(800, at(start, 10));
    state.sample(1000, at(start, 20));
    assert!(state.sample(1000, at(start, 30)).is_some());

    // 安静区间是 baseline - 40 = 980，回到 1015 并保持 3 次。
    state.sample(1015, at(start, 400));
    state.sample(1015, at(start, 410));
    state.sample(1015, at(start, 420));

    state.sample(800, at(start, 430));
    state.sample(800, at(start, 440));
    assert!(state.is_pressed());
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
fn rearm_timeout_forces_the_button_back_on() {
    let (mut state, start) = calibrated();

    state.sample(800, start);
    state.sample(800, at(start, 10));
    state.sample(1000, at(start, 20));
    assert!(state.sample(1000, at(start, 30)).is_some());

    // 电平一直停在低位，超过 rearm_timeout 后强制重新武装。
    for index in 0..10u64 {
        assert_eq!(state.sample(700, at(start, 200 + index * 10)), None);
    }
    state.sample(700, at(start, 5200));
    state.sample(700, at(start, 5210));
    assert!(state.is_pressed());
}
