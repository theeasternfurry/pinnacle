mod v0alpha1;
mod v1;

use super::StateFnSender;

#[derive(Clone)]
pub struct InputService {
    sender: StateFnSender,
}

impl InputService {
    pub fn new(sender: StateFnSender) -> Self {
        Self { sender }
    }
}
