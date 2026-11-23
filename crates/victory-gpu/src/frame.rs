//! Frame pacing: the title sets a frame interval and waits for each frame
//! to begin. Ours is a clock started by the first interval set, ticking
//! once per interval; the token is the frame number.
//!
//! shortcut: event offsets and display events are not told apart (all
//! waits end on the next tick); tie the clock to presentation once frames
//! reach the screen.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::object::{HRESULT, S_OK, This};

/// `HRESULT_FROM_WIN32(WAIT_TIMEOUT)`.
const TIMEOUT: HRESULT = 0x8007_0102_u32 as i32;
const SIXTIETH: Duration = Duration::from_micros(16_667);

struct Clock {
    start: Instant,
    interval: Duration,
}

static CLOCK: Mutex<Option<Clock>> = Mutex::new(None);
static FRAME: AtomicU64 = AtomicU64::new(0);

fn clock() -> std::sync::MutexGuard<'static, Option<Clock>> {
    CLOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Device method: `periods` intervals of `length` microseconds per frame.
pub(crate) extern "system" fn set_interval(
    _: This,
    _output: This,
    length: u32,
    periods: u32,
    _flags: u32,
) -> HRESULT {
    let interval = match u64::from(length) * u64::from(periods.max(1)) {
        0 => SIXTIETH,
        us => Duration::from_micros(us),
    };
    tracing::info!(?interval, "frame interval");
    let mut clock = clock();
    match clock.as_mut() {
        Some(c) => c.interval = interval,
        None => {
            *clock = Some(Clock {
                start: Instant::now(),
                interval,
            })
        }
    }
    S_OK
}

/// Device method: events are scheduled at every tick.
pub(crate) extern "system" fn schedule(
    _: This,
    _kind: u32,
    _offset: u32,
    _list: *const u8,
    _flags: u32,
) -> HRESULT {
    S_OK
}

/// How long until the next tick.
fn until_next_tick() -> Duration {
    let mut clock = clock();
    let c = clock.get_or_insert_with(|| Clock {
        start: Instant::now(),
        interval: SIXTIETH,
    });
    let elapsed = c.start.elapsed().as_nanos();
    let interval = c.interval.as_nanos().max(1);
    let next = (elapsed / interval + 1) * interval;
    Duration::from_nanos((next - elapsed) as u64)
}

/// Device method: blocks until the next tick, then hands out the frame's
/// token.
pub(crate) extern "system" fn wait(
    _: This,
    _kind: u32,
    timeout_ms: u32,
    _list: *const u8,
    _flags: u32,
    token: *mut u64,
) -> HRESULT {
    let wait = until_next_tick();
    let limit = Duration::from_millis(u64::from(timeout_ms));
    if wait > limit {
        std::thread::sleep(limit);
        return TIMEOUT;
    }
    std::thread::sleep(wait);
    let frame = FRAME.fetch_add(1, Ordering::Relaxed) + 1;
    if !token.is_null() {
        // SAFETY: checked non-null; the title passes its token.
        unsafe { token.write(frame) };
    }
    S_OK
}

/// Device method: no statistics are kept, so none are returned.
pub(crate) extern "system" fn statistics(
    _: This,
    _token: u64,
    _kinds: u32,
    count: *mut u32,
    _out: *mut u8,
) -> HRESULT {
    if !count.is_null() {
        // SAFETY: checked non-null.
        unsafe { count.write(0) };
    }
    S_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_end_on_ticks_and_count_frames() {
        let null = std::ptr::null();
        assert_eq!(set_interval(null, null, 2_000, 1, 0), S_OK);
        let mut first = 0;
        assert_eq!(wait(null, 0, 1_000, std::ptr::null(), 0, &mut first), S_OK);
        let before = Instant::now();
        let mut second = 0;
        assert_eq!(wait(null, 0, 1_000, std::ptr::null(), 0, &mut second), S_OK);
        assert!(second > first);
        assert!(before.elapsed() <= Duration::from_millis(50));
        assert_eq!(wait(null, 0, 0, std::ptr::null(), 0, &mut second), TIMEOUT);
    }
}
