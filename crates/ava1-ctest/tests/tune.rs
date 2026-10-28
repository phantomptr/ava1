#![cfg(unix)]
use ava1_ctest::*;

#[test]
fn workers_grow_until_files_per_second_stop_rising() {
    let mut t = CTune::new(4, 2, 16);
    let mut w = 4u8;
    for _ in 0..40 {
        let rate = (w as f64 * 200.0).min(900.0);
        w = t.step(rate, true);
    }
    assert_eq!(w, 5, "4→5 gains 12.5 %, 5→6 gains nothing");
}

#[test]
fn idle_workers_are_released_but_never_below_the_minimum() {
    let mut t = CTune::new(8, 2, 16);
    let mut w = 8u8;
    for _ in 0..60 {
        w = t.step(0.0, false);
    }
    assert_eq!(w, 2);
}
