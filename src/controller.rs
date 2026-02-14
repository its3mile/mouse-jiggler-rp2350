use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Duration;

/// Jiggle status
pub struct Status {
    pub enabled: bool,  // is jiggling enables
    pub next: Duration, // time until next jiggle
}

pub struct Controller {
    status: Mutex<CriticalSectionRawMutex, Status>, // jiggle status
    every: Duration,                                // how often a jiggle should occur
    cycle: Duration,                                // the jiggle countdown cycle duration
}

impl Controller {
    pub const fn new(initial_state: bool, every: Duration, cycle: Duration) -> Self {
        Self {
            status: Mutex::new(Status {
                enabled: initial_state,
                next: Duration::MIN,
            }),
            every: every,
            cycle: cycle,
        }
    }

    /// Return the jiggle state
    pub async fn is_enabled(&self) -> bool {
        let state: bool;
        {
            let unlocked = self.status.lock().await;
            state = (*unlocked).enabled;
            // Implicit release mutex at end of inner scope
        }
        state
    }

    /// Toggle the jiggle state, and return the new state
    pub async fn toggle(&self) -> bool {
        let state: bool;
        {
            let mut unlocked = self.status.lock().await;
            (*unlocked).enabled = !((*unlocked).enabled);
            state = (*unlocked).enabled;
            // set next to 0 so that a jiggle will occur immediately
            unlocked.next = Duration::MIN;
            // Implicit release mutex at end of inner scope
        }
        state
    }

    /// Reset the countdown
    pub async fn _reset(&self) {
        let mut unlocked = self.status.lock().await;
        unlocked.next = self.every;
    }

    /// Feed the countdown
    /// Returns true if it is time to jiggle
    pub async fn feed(&self) -> bool {
        let mut unlocked = self.status.lock().await;

        if !(unlocked.enabled) {
            return false;
        }

        match unlocked.next.checked_sub(self.cycle) {
            Some(remainder) => {
                // Decrease countdown
                unlocked.next = remainder;
                false
            }
            None => {
                // Reset the countdown
                unlocked.next = self.every;
                true
            }
        }
    }
}
