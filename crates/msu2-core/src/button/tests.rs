use super::*;

fn config() -> ButtonConfig {
    ButtonConfig {
        calibration_samples: 4,
        max_deviation: 800,
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
fn press_fires_on_the_press_edge() {
    let (mut state, start) = calibrated();

    assert_eq!(state.sample(800, start), None);
    // 去抖确认后立即上报，不等松手。
    let press = state.sample(800, at(start, 10)).expect("press");
    assert_eq!(press.trigger_raw, 800);
    assert!(state.is_pressed());

    // 按住期间不会重复触发。
    assert_eq!(state.sample(800, at(start, 100)), None);
    assert_eq!(state.sample(800, at(start, 200)), None);

    assert_eq!(state.sample(1000, at(start, 300)), None);
    assert_eq!(state.sample(1000, at(start, 310)), None);
    assert!(!state.is_pressed());

    // 继续停在松开状态也不会重复触发。
    assert_eq!(state.sample(1000, at(start, 800)), None);
    assert_eq!(state.sample(1000, at(start, 810)), None);
}

#[test]
fn strong_press_fires_on_the_first_sample() {
    let (mut state, start) = calibrated();

    // baseline 1020，下降 420 ≥ strong_press_delta 300，一次采样就确认。
    let press = state.sample(600, start).expect("strong press");
    assert_eq!(press.trigger_raw, 600);
    assert!(state.is_pressed());
}

#[test]
fn weak_press_still_needs_debounce() {
    let (mut state, start) = calibrated();

    // 800 低于按下阈值 870，但只降了 220，需要两次采样确认。
    assert_eq!(state.sample(800, start), None);
    assert!(!state.is_pressed());
    assert!(state.sample(800, at(start, 10)).is_some());
    assert!(state.is_pressed());
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

    // 900 高于按下阈值 870，但低于释放阈值 920，仍然算按住。
    state.sample(900, at(start, 100));
    state.sample(900, at(start, 110));
    assert!(state.is_pressed());

    state.sample(960, at(start, 200));
    state.sample(960, at(start, 210));
    assert!(!state.is_pressed());
}

#[test]
fn stays_disarmed_while_the_level_remains_low() {
    let (mut state, start) = calibrated();

    state.sample(800, start);
    assert!(state.sample(800, at(start, 10)).is_some());
    state.sample(1000, at(start, 20));
    state.sample(1000, at(start, 30));

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
    assert!(state.sample(800, at(start, 10)).is_some());
    state.sample(1000, at(start, 20));
    state.sample(1000, at(start, 30));

    // 安静区间是 baseline - 30 = 990，回到 1015 并保持 3 次。
    state.sample(1015, at(start, 400));
    state.sample(1015, at(start, 410));
    state.sample(1015, at(start, 420));

    state.sample(800, at(start, 430));
    assert!(state.sample(800, at(start, 440)).is_some());
    assert!(state.is_pressed());
}

#[test]
fn rejects_impossible_readings() {
    let (mut state, start) = calibrated();

    // baseline 是 1020，测试里 max_deviation 取 800：0 和 4095 都超出范围。
    assert_eq!(state.sample(0, start), None);
    assert_eq!(state.sample(4095, at(start, 10)), None);
    assert!(!state.is_pressed());

    // 正常范围的低值仍然能触发。
    state.sample(800, at(start, 20));
    state.sample(800, at(start, 30));
    assert!(state.is_pressed());
}

#[test]
fn rearm_timeout_forces_the_button_back_on() {
    let (mut state, start) = calibrated();

    state.sample(800, start);
    assert!(state.sample(800, at(start, 10)).is_some());
    state.sample(1000, at(start, 20));
    state.sample(1000, at(start, 30));

    // 电平一直停在低位，超过 rearm_timeout 后强制重新武装。
    for index in 0..10u64 {
        assert_eq!(state.sample(700, at(start, 200 + index * 10)), None);
    }
    // 700 降了 320，属于强信号，强制武装后一次采样就触发。
    assert!(state.sample(700, at(start, 5200)).is_some());
    assert!(state.is_pressed());
}
