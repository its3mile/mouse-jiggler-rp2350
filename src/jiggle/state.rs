use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

pub struct State {
    // Jiggle On/Off switch
    // This is wrapped in a mutex for convenience of sharing between tasks/coroutines
    mutex: Mutex<CriticalSectionRawMutex, bool>,
}

impl State {
    pub const fn new() -> Self {
        Self {
            mutex: Mutex::new(true),
        }
    }

    /// Return the jiggle state
    /// This waits on the jiggle state mutex
    pub async fn is_enabled(&self) -> bool {
        let state: bool;
        {
            let unlocked = self.mutex.lock().await;
            state = *unlocked;
            // Implicit release mutex at end of inner scope
        }
        state
    }

    /// Toggle the jiggle state, and return the new state
    /// This waits on the jiggle state mutex
    pub async fn toggle(&self) -> bool {
        let state: bool;
        {
            let mut unlocked = self.mutex.lock().await;
            *unlocked = !(*unlocked);
            state = *unlocked;
            // Implicit release mutex at end of inner scope
        }
        state
    }
}
