use heapless;

pub struct Movement {
    upper_limit: u8,
    lower_limit: u8,
    step: i8,
}

impl Movement {
    pub const fn new() -> Self {
        Self {
            upper_limit: 32,
            lower_limit: 6,
            step: 6,
        }
    }

    /// Generate a relative movement vector suitable for use in a mouse HID report
    /// The generated vector is a for a single axis, and returns to the starting position
    pub fn generate_vector<const N: usize>(&self, seed: u32, vec: &mut heapless::Vec<i8, N>) {
        // Scale rng_value into the range [LOWER, UPPER] inclusive.
        // Use 64-bit intermediate to avoid overflow and get decent distribution.
        let range: u32 = (self.upper_limit - self.lower_limit) as u32;
        let scaled: u32 = if seed == u32::MAX {
            range
        } else {
            ((seed as u64 * range as u64) / (u32::MAX as u64)) as u32
        };
        let x_u8 = (self.lower_limit as u32 + scaled) as u8;
        let mut remaining: i8 = x_u8 as i8;

        // Populate forward movement in STEP-sized chunks (last chunk may be smaller).
        while remaining > 0 && !vec.is_full() {
            let to_push: i8 = if remaining >= self.step {
                self.step
            } else {
                remaining
            };
            if vec.push(to_push).is_err() {
                break;
            }
            remaining -= to_push;
        }

        // Mirror back to origin. Iterate in reverse over current values and push negated values
        // until the vector is full.
        // Note: negating a value in the expected range (1..=16) is safe for i8.
        let clone = vec.clone();
        for &v in clone.iter().rev() {
            if vec.is_full() {
                break;
            }
            // push negated value; ignore push failure because we checked is_full above
            let _ = vec.push(-v);
        }
    }
}
