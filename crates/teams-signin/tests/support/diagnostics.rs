use serde_json::Value;
use std::sync::{Mutex, MutexGuard, Once, PoisonError};

static SERIAL: Mutex<()> = Mutex::new(());
static LINES: Mutex<Vec<Value>> = Mutex::new(Vec::new());
static INIT: Once = Once::new();
struct Capture;
impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "teams_signin::guard"
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let entry = serde_json::from_str(&record.args().to_string()).unwrap_or(Value::Null);
            LINES.lock().unwrap_or_else(PoisonError::into_inner).push(entry);
        }
    }
    fn flush(&self) {}
}
static CAPTURE: Capture = Capture;
pub fn capture() -> MutexGuard<'static, ()> {
    let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    INIT.call_once(|| {
        assert!(log::set_logger(&CAPTURE).is_ok());
        log::set_max_level(log::LevelFilter::Info);
    });
    LINES.lock().unwrap_or_else(PoisonError::into_inner).clear();
    serial
}
pub fn entries() -> Vec<Value> {
    LINES.lock().unwrap_or_else(PoisonError::into_inner).clone()
}
