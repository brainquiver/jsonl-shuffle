//! CPython's `random.Random`, as far as a shuffle needs it.
//!
//! The Mersenne Twister seeded from an integer the way CPython seeds it, `init_by_array` over
//! the 32-bit words of the seed; `getrandbits` built from those words the way CPython builds it;
//! `_randbelow` by rejection; and CPython's Fisher-Yates, from the last index down. A seed
//! therefore gives the order `random.Random(seed).shuffle(list(range(n)))` gives, so a file
//! shuffled here is the file a Python script shuffling the same lines with the same seed wrote.

const N: usize = 624;
const M: usize = 397;

pub struct Mt {
    state: [u32; N],
    index: usize,
}

impl Mt {
    /// Seeded as `random.Random(seed)` seeds from a non-negative integer.
    pub fn new(seed: u64) -> Self {
        let mut key = Vec::new();
        let mut rest = seed;
        while rest > 0 {
            key.push(rest as u32);
            rest >>= 32;
        }
        if key.is_empty() {
            key.push(0);
        }
        let mut mt = Mt { state: [0; N], index: N };
        mt.init_genrand(19_650_218);
        mt.init_by_array(&key);
        mt
    }

    fn init_genrand(&mut self, seed: u32) {
        self.state[0] = seed;
        for i in 1..N {
            let prev = self.state[i - 1];
            self.state[i] = 1_812_433_253u32.wrapping_mul(prev ^ (prev >> 30)).wrapping_add(i as u32);
        }
        self.index = N;
    }

    fn init_by_array(&mut self, key: &[u32]) {
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..N.max(key.len()) {
            let prev = self.state[i - 1];
            self.state[i] = (self.state[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                self.state[0] = self.state[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..N - 1 {
            let prev = self.state[i - 1];
            self.state[i] = (self.state[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_566_083_941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                self.state[0] = self.state[N - 1];
                i = 1;
            }
        }
        self.state[0] = 0x8000_0000;
        self.index = N;
    }

    fn twist(&mut self) {
        for i in 0..N {
            let y = (self.state[i] & 0x8000_0000) | (self.state[(i + 1) % N] & 0x7fff_ffff);
            let mut next = self.state[(i + M) % N] ^ (y >> 1);
            if y & 1 != 0 {
                next ^= 0x9908_b0df;
            }
            self.state[i] = next;
        }
        self.index = 0;
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.index >= N {
            self.twist();
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `getrandbits(k)` for 1 <= k <= 64: 32-bit words from the least significant up, the last
    /// one shifted down to the bits it has left.
    pub fn getrandbits(&mut self, k: u32) -> u64 {
        debug_assert!((1..=64).contains(&k));
        if k <= 32 {
            return u64::from(self.next_u32() >> (32 - k));
        }
        let low = u64::from(self.next_u32());
        let high = u64::from(self.next_u32() >> (64 - k));
        low | (high << 32)
    }

    /// `_randbelow(n)` for n > 0: draw `n.bit_length()` bits until the draw is below n.
    pub fn below(&mut self, n: u64) -> u64 {
        let k = 64 - n.leading_zeros();
        loop {
            let r = self.getrandbits(k);
            if r < n {
                return r;
            }
        }
    }

    /// `shuffle(x)`: for i from the last index down to 1, swap x[i] with x[below(i + 1)].
    pub fn shuffle<T>(&mut self, x: &mut [T]) {
        for i in (1..x.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            x.swap(i, j);
        }
    }
}
