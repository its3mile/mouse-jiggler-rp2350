use heapless;

pub struct Movement {
    upper_limit: u8,
    lower_limit: u8,
    step: i8,
}

impl Movement {
    pub const fn _new(upper_limit: u8, lower_limit: u8, step: i8) -> Self {
        Self {
            upper_limit: upper_limit,
            lower_limit: lower_limit,
            step: step as i8,
        }
    }

    pub const fn default() -> Self {
        Self {
            upper_limit: 32,
            lower_limit: 6,
            step: 6,
        }
    }

    fn xorshift32(&self, mut x: u32) -> u32 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    }

    fn scale_seed(&self, seed: u32) -> i8 {
        let range: u32 = (self.upper_limit - self.lower_limit) as u32;
        let scaled: u32 = if seed == u32::MAX {
            range
        } else {
            ((seed as u64 * range as u64) / (u32::MAX as u64)) as u32
        };

        let value = (self.lower_limit as u32 + scaled) as u8;
        value as i8
    }

    fn step_toward_zero(&self, remaining: i8, rng: u32, max_step: i8) -> i8 {
        if remaining == 0 {
            return 0;
        }

        let step = remaining.abs().min(max_step);
        let jitter = ((rng & 0x3) as i8) - 1; // [-1,0,1]
        let mut s = step + jitter;
        s = s.clamp(1, step); // ensure at least 1 and not overshoot

        if remaining < 0 { -s } else { s }
    }

    pub fn generate_vector<const N: usize>(&self, seed: u32) -> heapless::Vec<(i8, i8), N> {
        let mut vec: heapless::Vec<(i8, i8), N> = heapless::Vec::new();
        let mut rng = seed;

        // Total magnitude per axis
        let x_total = self.scale_seed(seed);
        let y_total = self.scale_seed(seed.rotate_left(13));

        // Determine direction: 50% chance to flip axes
        let flip = (rng & 1) == 1; // true => invert direction
        let (mut x_remaining, mut y_remaining) = if flip {
            (-x_total, -y_total)
        } else {
            (x_total, y_total)
        };

        // ---- Forward path with jitter + easing ----
        while (x_remaining != 0 || y_remaining != 0) && !vec.is_full() {
            rng = self.xorshift32(rng);

            let x_step = self.step_toward_zero(x_remaining, rng, self.step);
            let y_step = self.step_toward_zero(y_remaining, rng.rotate_left(5), self.step);

            x_remaining -= x_step;
            y_remaining -= y_step;

            vec.push((x_step, y_step)).ok();
        }

        // ---- Mirror back exactly ----
        let len = vec.len();
        for i in (0..len).rev() {
            if vec.is_full() {
                break;
            }

            let (x, y) = vec[i];
            vec.push((-x, -y)).ok();
        }

        vec
    }
}
