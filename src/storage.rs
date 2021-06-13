use anyhow::{anyhow, Result};
use bytes::Bytes;
use parking_lot::RwLock;
use std::{
    collections::{hash_map::RandomState, BTreeMap, HashMap},
    hash::{BuildHasher, Hash, Hasher},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};
use tracing::error;

/// A single cached entry
#[derive(Debug)]
struct Entry {
    value: Bytes,
    expiration_time: Instant,
    // This could be extended to contain
    // info about value compression
}

/// Cache key type
pub type Key = String;

/// A Shard is the final storage unit of the cache
/// it contains a hashmap of all the entries and also keeps
/// track of the TTl for all of the keys stored in the shard.
/// Every shard will be associated to a background task which
/// cleans up expired keys. This should not be constructed on
/// it's own but is an implementation detail of the Storage struct.
#[derive(Debug)]
struct Shard {
    map: HashMap<Key, Entry>,
    ttl_tracker: BTreeMap<Instant, Key>,
    cleanup_waker: Arc<Notify>,
    ttl: Duration,
}

impl Shard {
    /// Create a shard with pre allocated capacity
    fn with_capacity_and_ttl(capacity: usize, ttl: Duration) -> Self {
        Shard {
            map: HashMap::with_capacity(capacity),
            ttl_tracker: BTreeMap::new(),
            cleanup_waker: Arc::new(Notify::new()),
            ttl,
        }
    }

    /// Insert a key with an associated value.
    /// If the key already exists the value will be
    /// overwritten and the ttl will reset.
    fn insert(&mut self, key: Key, value: Bytes) {
        let expiration_time = Instant::now() + self.ttl;
        // Since all keys have the same ttl the cleanup task only
        // needs to be notified if the ttl_tracker was previously empty
        let should_notify = self.ttl_tracker.is_empty();
        self.ttl_tracker.insert(expiration_time, key.clone());
        if let Some(old_entry) = self.map.insert(
            key,
            Entry {
                value,
                expiration_time,
            },
        ) {
            // Remove the old entry from the ttl tracker to avoid
            // the new key from being removed prematurely
            self.ttl_tracker.remove(&old_entry.expiration_time);
        }
        if should_notify {
            self.cleanup_waker.notify_one();
        }
    }

    /// Get the value of a given key
    #[inline]
    fn get(&self, key: &Key) -> Option<Bytes> {
        // The clone is very cheap and is not a deep copy
        self.map.get(key).map(|entry| entry.value.clone())
    }

    /// Remove all expired keys from the shard storage and return the time
    /// when the next key will expire if there is still keys left in the storage.
    fn remove_exipired(&mut self) -> Option<Instant> {
        let current_time = Instant::now();
        while let Some((&expiration_time, key)) = self.ttl_tracker.iter().next() {
            if current_time >= expiration_time {
                self.map.remove(key);
                self.ttl_tracker.remove(&expiration_time);
            } else {
                return Some(expiration_time);
            }
        }
        None
    }
}

/// The storage of the cache service. The storage is built with concurrency in mind and is thus
/// sharded to reduce lock contention. It uses the default std hasher which provides some
/// protection agains HashDoS attacks. It will set the same ttl to all keys.
#[derive(Debug)]
pub struct Storage {
    shift: usize,
    shards: Box<[Arc<RwLock<Shard>>]>,
    hasher: RandomState,
    shutting_down: Arc<AtomicBool>,
}

const PTR_BITS: usize = std::mem::size_of::<usize>() * 8;
const METADATA_BITS: usize = 7;

impl Storage {
    /// Create a new storage with a given capacity, shard count and ttl. If no shard count is given
    /// it will default to a number based on the number of cores available. This will also spawn a
    /// cleanup task for each storage shard that cleans up expired keys in the background
    pub fn new(capacity: usize, ttl: Duration, num_shards: Option<usize>) -> Result<Self> {
        // num_shards is always a power of two which is important for determining which shard
        // a given key should be distributed to.
        let num_shards = num_shards.unwrap_or_else(|| (num_cpus::get() * 4).next_power_of_two());
        if !num_shards.is_power_of_two() {
            return Err(anyhow!("Shard count must be a power of two"));
        }
        // The shift is used to distribute key hashes among the different shards.
        // Shifting this amount to the right will make the number fit within the index range of the
        // shards. See more in the `deterimne_shard` method.
        let shift = PTR_BITS - num_shards.trailing_zeros() as usize;

        let shard_capacity = capacity / num_shards;
        let shutting_down = Arc::new(AtomicBool::new(false));

        let shards = (0..num_shards)
            .map(|_| {
                let shard = Shard::with_capacity_and_ttl(shard_capacity, ttl);
                let waker = shard.cleanup_waker.clone();
                let shard = Arc::new(RwLock::new(shard));
                // start the clean up task for each shard
                tokio::task::spawn(cleanup_expired_keys_task(
                    waker,
                    shard.clone(),
                    shutting_down.clone(),
                ));
                shard
            })
            .collect();
        Ok(Storage {
            shift,
            shards,
            hasher: RandomState::new(),
            shutting_down,
        })
    }

    /// Hash the key as an usize to be used in shard indexing
    fn hash_usize(&self, key: &Key) -> usize {
        let mut hasher = self.hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish() as usize
    }

    /// Deterimines the shard of a given key hash, this uses the same strategy as the Dashmap crate
    #[inline(always)]
    fn deterimine_shard(&self, hash: usize) -> usize {
        // The left shift leaves out the metadata bits used by the std Hashmap implementation.
        // More information can be found here:
        // https://abseil.io/about/design/swisstables#lookup-optimizations
        //
        // Each hash is then (right) shifted the calculated shift amount to make sure the hash is always fits within the
        // index range of the shards. Conceptually it's almost a cheap modolu operation of the hash.
        (hash << METADATA_BITS) >> self.shift
    }

    /// Insert a key associated to a given value in to the storage. If already present the value will be overwritten and the ttl will be reset.
    pub fn insert(&self, key: Key, value: Bytes) {
        let hash = self.hash_usize(&key);
        let index = self.deterimine_shard(hash);
        if let Some(shard) = self.shards.get(index) {
            let mut shard = shard.write();
            shard.insert(key, value);
        } else {
            error!(
                "Shard index: {} is out of bounds! Number of shards: {}. This should never happen",
                index,
                self.shards.len()
            );
        }
    }

    /// Get the value of a key if it's present
    pub fn get(&self, key: &Key) -> Option<Bytes> {
        let hash = self.hash_usize(&key);
        let index = self.deterimine_shard(hash);
        if let Some(shard) = self.shards.get(index) {
            shard.read().get(key)
        } else {
            error!(
                "Shard index: {} is out of bounds! Number of shards: {}. This should never happen",
                index,
                self.shards.len()
            );
            None
        }
    }
}

impl Drop for Storage {
    /// Make the shutdown process a bit cleaner by shutting down the clean up tasks
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        for shard in self.shards.iter() {
            shard.read().cleanup_waker.notify_one();
        }
    }
}

/// Removes expired keys for a given shard, this is will sleep until woken up or untill the next
/// key in a given shard expires
async fn cleanup_expired_keys_task(
    waker: Arc<Notify>,
    shard: Arc<RwLock<Shard>>,
    shutting_down: Arc<AtomicBool>,
) {
    // This prevents the task from continuing forever which makes the shutdown process a bit
    // cleaner
    while !shutting_down.load(Ordering::Acquire) {
        let maybe_next_expiration;
        {
            let mut locked = shard.write();
            maybe_next_expiration = locked.remove_exipired();
        }
        if let Some(next_expiration) = maybe_next_expiration {
            // Wait until either notified by the Storage struct or when the next key expires
            tokio::select! {
                _ = tokio::time::sleep_until(next_expiration) => {}
                _ = waker.notified() => {}
            }
        } else {
            waker.notified().await;
        }
    }
}
