use bombechat_modem::{AudioError, AudioIo, FakeAudio, QuietGate};

#[test]
fn waits_for_quiet() {
    let mut g = QuietGate::new(40.0, 2, 90);
    g.enqueue(0);
    g.observe(47.0, 0);
    assert_eq!(g.due(0), None);
    g.observe(38.0, 1000);
    assert_eq!(g.due(1000), Some(0));
}

#[test]
fn repeats_twice_ninety_seconds_apart() {
    let mut g = QuietGate::new(40.0, 2, 90);
    g.enqueue(7);
    g.observe(10.0, 0);

    // First play, as soon as the room is quiet.
    assert_eq!(g.due(0), Some(7));

    // The first repeat is not due before the 90 s gap, even though the
    // room stays quiet the whole time.
    assert_eq!(g.due(1), None);
    assert_eq!(g.due(89_999), None);

    // Due again exactly 90 s after the first play.
    assert_eq!(g.due(90_000), Some(7));

    // The second repeat follows the same rule, timed from the first
    // repeat.
    assert_eq!(g.due(90_001), None);
    assert_eq!(g.due(179_999), None);
    assert_eq!(g.due(180_000), Some(7));

    // Three plays total (1 + repeats(2)); never due again.
    assert_eq!(g.due(180_001), None);
    assert_eq!(g.due(270_000), None);
    assert_eq!(g.due(10_000_000), None);
}

#[test]
fn denied_mic_is_reported_not_panicked() {
    let mut a = FakeAudio::denied();
    assert_eq!(
        a.start_input(Box::new(|_| {})),
        Err(AudioError::PermissionDenied)
    );
}

#[test]
fn nothing_due_before_any_observation() {
    let mut g = QuietGate::new(40.0, 0, 90);
    g.enqueue(1);
    assert_eq!(g.due(0), None);
    assert_eq!(g.due(1_000_000), None);

    // Still nothing once the room is loud.
    g.observe(90.0, 1_000_001);
    assert_eq!(g.due(1_000_001), None);
}

#[test]
fn fifo_order_of_two_items() {
    let mut g = QuietGate::new(40.0, 0, 90);
    g.enqueue(1);
    g.enqueue(2);
    g.observe(10.0, 0);

    // Both are due for their one and only play, in enqueue order, one per
    // call.
    assert_eq!(g.due(0), Some(1));
    assert_eq!(g.due(0), Some(2));
    assert_eq!(g.due(0), None);
}

#[test]
fn repeat_waits_for_quiet_after_the_gap() {
    let mut g = QuietGate::new(40.0, 1, 90);
    g.enqueue(5);
    g.observe(10.0, 0);
    assert_eq!(g.due(0), Some(5));

    // The gap has elapsed, but the room is loud: the repeat still waits.
    g.observe(55.0, 90_000);
    assert_eq!(g.due(90_000), None);
    g.observe(50.0, 95_000);
    assert_eq!(g.due(95_000), None);

    // Quiet again, after the gap: now it plays.
    g.observe(30.0, 96_000);
    assert_eq!(g.due(96_000), Some(5));

    // One repeat used up; never due again.
    g.observe(10.0, 500_000);
    assert_eq!(g.due(500_000), None);
}

#[test]
fn send_under_can_be_raised_for_a_loud_room() {
    let mut g = QuietGate::new(40.0, 0, 90);
    g.enqueue(0);
    g.observe(43.0, 0);
    assert_eq!(g.due(0), None, "43 dB is not under 40");
    g.set_send_under(45.0);
    assert_eq!(g.send_under(), 45.0);
    assert_eq!(g.due(10), Some(0), "43 dB is under 45");
}
