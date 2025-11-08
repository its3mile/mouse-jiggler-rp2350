use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

// Jiggle mutex i.e., on and off switch
// Enable jiggle by default
type StateType = Mutex<CriticalSectionRawMutex, bool>;
static STATE_MUTEX: StateType = Mutex::new(true);

pub struct State {}

impl State {
    /// Return the jiggle state
    /// This waits on the jiggle state mutex
    pub async fn is_enabled() -> bool {
        let state: bool;
        {
            let unlocked = STATE_MUTEX.lock().await;
            state = *unlocked;
            // Implicit release mutex at end of inner scope
        }
        state
    }

    /// Toggle the jiggle state, and return the new state
    /// This waits on the jiggle state mutex
    pub async fn toggle() -> bool {
        let state: bool;
        {
            let mut unlocked = STATE_MUTEX.lock().await;
            *unlocked = !(*unlocked);
            state = *unlocked;
            // Implicit release mutex at end of inner scope
        }
        state
    }
}
