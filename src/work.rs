use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// Cooperative cancellation also covers Rust work between SQLite statements.
#[derive(Clone, Default)]
pub struct Work {
    cancelled: Arc<AtomicBool>,
    deadline: Option<Instant>,
}
impl Work {
    pub fn until(deadline: Instant) -> Self {
        Self {
            deadline: Some(deadline),
            ..Self::default()
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn check(&self) -> anyhow::Result<()> {
        if self.cancelled.load(Ordering::Relaxed)
            || self.deadline.is_some_and(|d| Instant::now() > d)
        {
            return Err(crate::fault(
                408,
                "요청이 취소되었거나 시간이 초과되었습니다. 다시 시도하세요.",
            ));
        }
        Ok(())
    }
}

pub struct CancelOnDrop(pub Work);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
