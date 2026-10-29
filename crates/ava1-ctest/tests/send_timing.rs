//! Review L2: the download sender's stage-timer line is opt-in, not printed on every job.
#![cfg(unix)]

use ava1_ctest::ffi;

#[test]
fn the_stage_timer_line_is_off_unless_asked_for() {
    // Its own test binary, so setting the environment cannot race another test.
    std::env::remove_var("AVA1_TIMING");
    assert_eq!(
        unsafe { ffi::ava1_send_timing_enabled() },
        0,
        "silent by default"
    );
    std::env::set_var("AVA1_TIMING", "1");
    assert_eq!(unsafe { ffi::ava1_send_timing_enabled() }, 1);
    std::env::remove_var("AVA1_TIMING");
    assert_eq!(unsafe { ffi::ava1_send_timing_enabled() }, 0);
}
