//! glib's GHashTable (ghash.c, glib 2.88) with g_str_hash, as far as update-mime-database uses
//! it: insert, lookup and iteration, which must visit the entries in the same order, since that
//! order ends up in its output. No removal, so there are no tombstones and the table never
//! shrinks (kept as in glib anyway).
//!
//! Keys are stored in one byte buffer, values are `Copy`.

const MIN_SHIFT: u32 = 3;
const UNUSED: u32 = 0;

const PRIME_MOD: [u32; 32] = [
    1, 2, 3, 7, 13, 31, 61, 127, 251, 509, 1021, 2039, 4093, 8191, 16381, 32749, 65521, 131071,
    262139, 524287, 1048573, 2097143, 4194301, 8388593, 16777213, 33554393, 67108859, 134217689,
    268435399, 536870909, 1073741789, 2147483647,
];

/// g_str_hash: djb2 over `signed char`s
pub fn str_hash(key: &[u8]) -> u32 {
    key.iter().fold(5381u32, |h, &b| {
        (h << 5).wrapping_add(h).wrapping_add(b as i8 as i32 as u32)
    })
}

fn is_real(h: u32) -> bool {
    h >= 2
}

pub struct GHash<V> {
    size: u32,
    modulo: u32,
    mask: u32,
    nnodes: u32,
    noccupied: u32,
    hashes: Vec<u32>,
    /// (start, len) in `key_buf`
    keys: Vec<(u32, u32)>,
    values: Vec<V>,
    key_buf: Vec<u8>,
}

impl<V: Copy + Default> GHash<V> {
    /// `key_bytes`: capacity of the key buffer
    pub fn new(key_bytes: usize) -> Self {
        let mut t = Self {
            size: 0,
            modulo: 0,
            mask: 0,
            nnodes: 0,
            noccupied: 0,
            hashes: Vec::new(),
            keys: Vec::new(),
            values: Vec::new(),
            key_buf: Vec::with_capacity(key_bytes),
        };
        t.set_shift(MIN_SHIFT);
        t.grow_arrays();
        t
    }

    fn grow_arrays(&mut self) {
        let n = self.size as usize;
        self.hashes.resize(n, UNUSED);
        self.keys.resize(n, (0, 0));
        self.values.resize(n, V::default());
    }

    fn set_shift(&mut self, shift: u32) {
        assert!(
            shift <= 31,
            "adding more entries to hash table would overflow"
        );
        self.size = 1 << shift;
        self.modulo = PRIME_MOD[shift as usize];
        self.mask = self.size - 1;
    }

    fn hash_to_index(&self, hash: u32) -> u32 {
        hash.wrapping_mul(11) % self.modulo
    }

    fn key(&self, i: usize) -> &[u8] {
        let (s, l) = self.keys[i];
        &self.key_buf[s as usize..(s + l) as usize]
    }

    /// g_hash_table_lookup_node: the bucket of `key`, or where it would go, and its hash.
    fn lookup_node(&self, key: &[u8]) -> (usize, u32) {
        let mut hash = str_hash(key);
        if !is_real(hash) {
            hash = 2;
        }
        let mut index = self.hash_to_index(hash);
        let mut step = 0;
        let mut first_tombstone = None;
        loop {
            let node_hash = self.hashes[index as usize];
            if node_hash == UNUSED {
                break;
            }
            if node_hash == hash {
                if self.key(index as usize) == key {
                    return (index as usize, hash);
                }
            } else if !is_real(node_hash) && first_tombstone.is_none() {
                first_tombstone = Some(index as usize);
            }
            step += 1;
            index = (index + step) & self.mask;
        }
        (first_tombstone.unwrap_or(index as usize), hash)
    }

    pub fn get(&self, key: &[u8]) -> Option<V> {
        let (i, _) = self.lookup_node(key);
        is_real(self.hashes[i]).then(|| self.values[i])
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// g_hash_table_insert: an existing key keeps its key and gets the new value.
    pub fn insert(&mut self, key: &[u8], value: V) {
        let (i, hash) = self.lookup_node(key);
        let old_hash = self.hashes[i];
        if is_real(old_hash) {
            self.values[i] = value;
            return;
        }
        self.hashes[i] = hash;
        self.keys[i] = (self.key_buf.len() as u32, key.len() as u32);
        self.key_buf.extend_from_slice(key);
        self.values[i] = value;
        self.nnodes += 1;
        if old_hash == UNUSED {
            self.noccupied += 1;
            self.maybe_resize();
        }
    }

    fn maybe_resize(&mut self) {
        let (size, noccupied) = (self.size, self.noccupied);
        if (size > 1 << MIN_SHIFT && (size - 1) / 4 >= self.nnodes)
            || size <= noccupied + noccupied / 16
        {
            self.resize();
        }
    }

    /// g_hash_table_resize: in place, relocating entries with a bitmap of settled buckets.
    fn resize(&mut self) {
        let old_size = self.size;
        let n = self.nnodes + self.nnodes / 3;
        self.set_shift((32 - n.leading_zeros()).max(MIN_SHIFT));
        let new_size = self.size;
        if new_size > old_size {
            self.grow_arrays();
        }
        let mut settled = vec![0u32; (old_size.max(new_size) as usize).div_ceil(32)];
        let is_set = |b: &[u32], i: u32| b[i as usize / 32] >> (i % 32) & 1 == 1;
        for i in 0..old_size {
            let mut node_hash = self.hashes[i as usize];
            if !is_real(node_hash) {
                self.hashes[i as usize] = UNUSED;
                continue;
            }
            if is_set(&settled, i) {
                continue;
            }
            self.hashes[i as usize] = UNUSED;
            let mut key = self.keys[i as usize];
            let mut value = self.values[i as usize];
            loop {
                let mut index = self.hash_to_index(node_hash);
                let mut step = 0;
                while is_set(&settled, index) {
                    step += 1;
                    index = (index + step) & self.mask;
                }
                settled[index as usize / 32] |= 1 << (index % 32);
                let index = index as usize;
                let replaced = self.hashes[index];
                self.hashes[index] = node_hash;
                if !is_real(replaced) {
                    self.keys[index] = key;
                    self.values[index] = value;
                    break;
                }
                node_hash = replaced;
                key = std::mem::replace(&mut self.keys[index], key);
                value = std::mem::replace(&mut self.values[index], value);
            }
        }
        if new_size < old_size {
            self.hashes.truncate(new_size as usize);
            self.keys.truncate(new_size as usize);
            self.values.truncate(new_size as usize);
        }
        self.noccupied = self.nnodes;
    }

    /// g_hash_table_foreach order
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], V)> + '_ {
        (0..self.size as usize)
            .filter(|&i| is_real(self.hashes[i]))
            .map(move |i| (self.key(i), self.values[i]))
    }

    pub fn len(&self) -> usize {
        self.nnodes as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash() {
        assert_eq!(str_hash(b""), 5381);
        assert_eq!(str_hash(b"a"), 5381 * 33 + 97);
        // signed char: 0xc3 is -61
        assert_eq!(str_hash(&[0xc3]), (5381u32 * 33).wrapping_sub(61));
    }

    #[test]
    fn insert_lookup() {
        let mut t = GHash::new(0);
        for i in 0..1000u32 {
            t.insert(format!("k{i}").as_bytes(), i);
        }
        t.insert(b"k5", 55);
        assert_eq!(t.len(), 1000);
        assert_eq!(t.get(b"k5"), Some(55));
        assert_eq!(t.get(b"k999"), Some(999));
        assert_eq!(t.get(b"nope"), None);
        assert_eq!(t.iter().count(), 1000);
    }
}
