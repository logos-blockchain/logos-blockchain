# KV-backed, fork-safe nullifier IMT — design plan (v5)

Status:

- Phases 1–3 are implemented: `merkle/kv-store` and `merkle/imt/src/store/`.
- Phases 4–6 are not started. `NoteSet` still uses the in-memory
  `IndexedMerkleTree`.

Changes since v4, both found while implementing:

- Reads filter base index entries by `leaf_index < len`.
- Garbage collection waits for a retention floor, so that epoch snapshots
  below the LIB stay readable. See §3, "Garbage collection and retained
  snapshots".

Decisions taken:

- Merkle nodes are persisted **copy-on-write**: immutable, versioned nodes on
  disk. A block writes new keys and never overwrites one. Only a small index
  of the nullifiers inserted since the LIB is kept in memory.
- Storage is abstracted behind a **generic byte KV trait**.
- Storage I/O errors are **propagated as `Result`**.
- Stored items and keys are **named structs**, never tuples.

---

## 0. Current state

- `merkle/imt/src/lib.rs` keeps everything in memory in `rpds` structures:
  `leaves`, a `sorted` index and a `DynamicMerkleTree`. Its only consumer is
  `core/src/mantle/note_set.rs` (`NullifierImt`), used by two note sets,
  `LEDGER_SET` and `SDP_SET`.
- `Ledger` keeps a `HashTrieMap<Id, LedgerState>` with one cloned state per
  block (`ledger/src/lib.rs:149`). Forks are correct today only because the
  `rpds` versions are persistent.
- An insert appends a leaf at `len`, rewrites the low leaf's `next_nullifier` /
  `next_index`, and rehashes the paths of both leaves. Positions are
  append-only; leaf contents and nodes are not. In a shared KV, every one of
  those keys collides between sibling forks, the appended leaf position
  included: two forks put different nullifiers at the same index, and a
  nullifier of one fork must be invisible to the other. The low leaf rewrite
  is mandated by the spec, since the leaf hash under `NULLIFIER_IMT_LEAF_V1`
  commits to `next_*`, and the circuits rely on it.
- Chain-service persists `lib_ledger_state`
  (`services/chain/chain-service/src/states.rs:14`), which today contains every
  leaf, and on restart replays the blocks from the LIB to the tip.
- `PrunedBlocks` (`consensus/cryptarchia-engine/src/lib.rs:869`) already
  separates `stale_blocks` (forks that diverged below the new LIB) from
  `immutable_blocks` (canonical blocks deeper than the LIB). Today
  `prune_ledger_states` (`services/chain/chain-service/src/lib.rs:634`) treats
  both the same way, through the call sites at `lib.rs:474` and `lib.rs:662`.
- The memory problem: the set grows forever, so memory, the size of the
  serialized state and the time to rebuild it at startup all grow with chain
  age.

---

## 1. `merkle/kv-store` crate (new)

```rust
pub trait KvStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;
    fn get(&self, key: &[u8]) -> Result<Option<Bytes>, Self::Error>;
    /// Greatest entry with key < `key` that starts with `prefix` (RocksDB seek_for_prev).
    fn get_prev(&self, prefix: &[u8], key: &[u8]) -> Result<Option<KvEntry>, Self::Error>;
    /// Every entry whose key starts with `prefix`, in key order.
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error>;
    /// Applied atomically.
    fn write_batch(&self, batch: WriteBatch) -> Result<(), Self::Error>;
}

pub struct KvEntry { pub key: Bytes, pub value: Bytes }

pub enum WriteOp {
    Put(KvEntry),
    Delete { key: Bytes },
    DeletePrefix { prefix: Bytes },   // RocksDB delete_range
}

pub struct WriteBatch { ops: Vec<WriteOp> }
```

- The trait is synchronous on purpose. Ledger state transitions are
  synchronous, so the async, `&mut self` `StorageBackend` of the storage
  service does not fit.
- `scan_prefix` returns a `Vec` because it only reads small sets (one block's
  stale records). It can become an iterator later.
- `MemKv` is a `BTreeMap` behind a `RwLock`, for tests and benches, with
  optional read counters.
- `Prefixed<S>` namespaces each note set's keys under `imt/{note_set_index}/…`.
- A conformance suite runs against any implementation: edge cases of
  `get_prev`, scanning and deleting by prefix, and batch atomicity.

---

## 2. Storage layout (`merkle/imt`)

### Identifiers

```rust
pub struct Version(pub u64);        // one per committed block, allocated by Ledger
pub struct LeafIndex(pub u64);
pub struct NodePosition { pub level: u8, pub index: u64 }   // level 0 = leaves, HEIGHT = root
```

### Keys

Each key encodes itself through
`trait StoreKey { const TAG: u8; fn encode(&self) -> Bytes; }`. Integers are
big-endian, and `Version` comes first wherever whole versions are deleted by
prefix.

| Key struct | Encoding | Value struct | Lifetime |
|---|---|---|---|
| `NodeKey { version, position }` | `H ‖ version ‖ level ‖ index` | `StoredNode` | immutable, removed by GC |
| `StaleKey { replaced_by: Version, node: NodeKey }` | `S ‖ replaced_by ‖ node` | `StaleMarker` (empty struct) | until `replaced_by` is garbage collected or discarded |
| `GcKey { end_len, version }` | `G ‖ end_len ‖ version` | `GcMarker` (empty struct) | from the finalization of `version` until it is garbage collected |
| `NullifierKey { nullifier }` | `N ‖ nullifier_be32` | `IndexEntry { leaf_index }` | finalized nullifiers only, append-only |
| `BlockVersionKey { block_id }` | `B ‖ block_id` | `Version` | until the block is finalized or discarded |
| `LiveVersionKey { version }` | `L ‖ version` | `LiveVersion { block_id }` | until the block is finalized or discarded |
| `MetaKey` | `M` | `BaseMeta { len, root: Child, block_id }` | overwritten at each finalize (single writer) |
| `NextVersionKey` | `C` | `Version` | overwritten at each commit (single writer) |

`NullifierKey` is big-endian, so its byte order is the integer order of the
nullifiers, near the modulus included.

### Values

```rust
pub struct Child { pub hash: Fr, pub version: Option<Version> }   // None = empty subtree (hash = empty_subtree_root)

pub enum StoredNode {
    Internal { left: Child, right: Child },   // own hash = compress(left.hash, right.hash)
    Leaf { leaf: NullifierLeaf },             // hash = leaf.hash::<H>()
}

pub struct NullifierLeaf { pub nullifier: Fr, pub next_nullifier: Fr, pub next_index: LeafIndex }
```

- A parent stores the hashes of its children, so a walk from the root to a
  leaf reads exactly H nodes and finds every sibling of the `path` on the way.
- A leaf node stores the whole `NullifierLeaf`, `next_*` included. Since nodes
  are versioned, rewriting the low leaf just writes a new version of its node,
  and no successor lookup is needed.
- The leaf hash is unchanged: it hashes `Fr::from(next_index.0)`.
- A finalized tree takes about 2N nodes (about 100 B each) plus N index
  entries (about 50 B each), so **about 250 B raw per nullifier**.
- Nodes are immutable, so a process-wide LRU `NodeCache` keyed by `NodeKey`
  would never have to be invalidated, only evicted. **Not implemented yet**:
  the backend's own block cache serves the hot top levels for now.
- The `B`, `L` and `C` keys belong to the `VersionRegistry`, shared by the
  trees of a ledger; the other keys to each tree, under its own namespace.

---

## 3. In-memory state and lifecycle

Implemented in `merkle/imt/src/store/`.

```rust
#[derive(Clone)]                                   // O(1)
pub struct Imt<S, H, const HEIGHT: usize> {
    store: S,                                       // KvStore + Clone (an Arc, a Prefixed, a DynKvStore)
    root: Child,                                    // committed root of this state
    len: u64,
    index: RedBlackTreeMapSync<NullifierKey, IndexEntry>,  // nullifiers since the KV base, pending ones included
    unfinalized: QueueSync<BlockRecord>,            // committed blocks of this state above the base, oldest first
    pending: Option<Pending>,                       // the block being built, between insert and commit
}

struct BlockRecord { version: Version, end_len: u64, nullifiers: VectorSync<NullifierKey> }

struct Pending {
    leaves: RedBlackTreeMapSync<LeafIndex, NullifierLeaf>,  // staged leaves
    nullifiers: VectorSync<NullifierKey>,
    sealed: Option<Sealed>,                                 // None after an insert, until `seal`
}

struct Sealed {
    nodes: HashTrieMapSync<NodePosition, PendingNode>,
    replaced: VectorSync<NodeKey>,                          // committed nodes the block replaces
    root: Fr,
}

enum PendingChild { Committed(Child), Pending { hash: Fr } }   // Pending = written by this block
enum PendingNode {
    Internal { left: PendingChild, right: PendingChild },
    Leaf { leaf: NullifierLeaf },
}

pub struct Committed<I> { pub imt: I, pub writes: WriteBatch }
pub struct Finalized<I> { pub imt: I, pub writes: WriteBatch }
pub struct Snapshot { pub len: u64, pub root: Child }
pub struct LowLeaf { pub index: LeafIndex, pub leaf: NullifierLeaf }
```

Every operation returns the new state and, where it writes, the
`WriteBatch` to apply. Nothing writes to the store except `Imt::initialize`
(genesis). The caller applies the batch, together with its own writes, before
it uses the returned state, so a failed write leaves memory and store as they
were.

### Reads (any state, committed or pending)

Reads filter by length. Leaf indices only grow along the canonical chain, so
the base index entries of a canonical state are exactly those with
`leaf_index < len`. A state below the base (an epoch snapshot) is therefore
fully described by its `len` and `root`.

- `contains(n)`: the `index` overlay, then `NullifierKey` in the KV, counted
  only if its `leaf_index < len`.
- `low_leaf(n) -> Option<LowLeaf>`: the greater of the overlay's
  `range(..n).next_back()` and the KV's `get_prev(N, n)`. Base entries at or
  past `len` are skipped by repeating `get_prev`. Then the leaf is read from
  `pending`, or by walking the committed nodes from `root`.
- `leaf(i)` / `path(i)`: a walk of H levels from `root`, or from the sealed
  pending root.
- `root()`: O(1) once sealed; before `seal`, the pending block is hashed on the
  fly, without keeping the result.

### Building a block (inside `try_update`, no writes)

- `insert(n)`:
  1. Reject zero, a duplicate, or a full tree.
  2. Find the low leaf.
  3. Stage the rewritten low leaf `{next = n, next_index = len}` and the new
     leaf `{n, low.next_*}` in `pending.leaves`.
  4. Add `n` to the overlay.
- `seal()`:
  1. Collect the dirty positions, level by level.
  2. Read each dirty committed internal node once, from the root down, which
     gives the untouched siblings.
  3. Hash bottom-up, each node once.

  The pending root becomes visible to `root()` and `path()`. This replaces
  today's `flush`. Inserting after a seal drops the seal.

### Commit: `commit(version) -> Committed`, called by `Ledger::commit_update`

- `Put(NodeKey { version, position })` for every dirty position.
  `PendingChild::Pending` is stored as `Child { version: Some(version) }`.
- For every dirty position that held a committed node,
  `Put(StaleKey { replaced_by: version, node: old_key })`.
- The new state has `root = Child { version: Some(version), .. }`,
  `BlockRecord { version, end_len: len, nullifiers }` pushed onto
  `unfinalized`, and `pending = None`.
- A state without a pending block commits to itself, with no writes and no
  record (an empty block).
- Siblings get distinct versions, so **they never write the same key**.
- Versions come from the `VersionRegistry` (`B`/`L`/`C` keys). A block that
  already has a version (a replay at restart) gets it back, and the recomputed
  nodes are byte-identical, so the writes are idempotent.

### Finalize: `finalize(block_id) -> Finalized`, run on the LIB state

For each `BlockRecord` in `unfinalized`:

1. `Put` its nullifiers into the base index.
2. `Put(GcKey { end_len, version })`, queuing its replaced nodes for garbage
   collection.

Then `Put(MetaKey → BaseMeta)`. The finalized state has an empty overlay and
no records. Finalize reads nothing from the store. The caller releases the
registry entries of the finalized blocks with
`VersionRegistry::release_writes`.

### Garbage collection and retained snapshots

Each `LedgerState` keeps the epoch snapshot of its note sets
(`epoch_state.note_sets`). Leader proofs read the low leaf and path of a
nullifier in it (`pol_wallet_inputs`, `core/src/proofs/leader_proof.rs`).
That snapshot is a canonical state, usually below the LIB. So the nodes
replaced after it must stay until no retained state is older than the
replacing block.

`Imt::collect_garbage(store, floor_len) -> WriteBatch` works through the `G`
keys in `end_len` order and stops at the first one with `end_len > floor_len`.
For each collected block it:

- `Delete`s every node named by its `S ‖ version` records;
- deletes those records;
- deletes the `G` key.

`floor_len` is the smallest `len` among the retained states (the LIB and the
epoch snapshots in use), computed by the ledger for each note set.

- A canonical state with `len ≥ end_len` is at or after that block, so it
  never reads the nodes the block replaced. That is why deleting at
  `end_len ≤ floor_len` is safe.
- GC is per finalized block, never by version range: versions follow commit
  order, not chain order.
- It runs as its own batch after the finalize batch, so the `G` keys it reads
  are already written. A crash in between only delays the collection.
- Nodes created and replaced inside the retention window, with no retained
  state between the two blocks, wait until the floor passes their replacing
  block. The disk overhead is bounded by the nodes the canonical chain replaced
  since the oldest retained snapshot.

`snapshot() -> Snapshot { len, root }` and
`Imt::from_snapshot(store, Snapshot)` read a canonical state at or below the
base again, for instance after a restart. A snapshot above the base is
rejected with `BaseMismatch`.

### Lazy rebase of the overlay

`rebase()` reads `BaseMeta` and drops the records with
`end_len ≤ base.len`, together with their nullifiers. They are in the base
index, so this only frees memory. `prepare_update` rebases the parent state it
starts from.

### Fork pruning

The cryptarchia engine prunes only when the LIB advances (`update_lib`,
`consensus/cryptarchia-engine/src/lib.rs:558`). `prune_stale_forks` then
removes **every** non-canonical block of each fork that diverged below the new
LIB, walking from the fork tip down to the LCA (`prune_fork`, `:653`). Forks
that diverge at or above the LIB are kept. So a discarded block never has a
surviving descendant, and no live state can reference a discarded node. The
`online()` transition (`:718`) returns `PrunedBlocks` the same way.

What a pruned fork owns, and what discarding it does:

| Item | Owner | On discard |
|---|---|---|
| `NodeKey { version: v, .. }` of each fork block | the fork only | **deleted**: `DeletePrefix(H ‖ v)` |
| `StaleKey { replaced_by: v, .. }` | the fork only | **deleted**: `DeletePrefix(S ‖ v)`. The node it points to is **not** deleted |
| Nodes read but not written (written by canonical ancestors) | the canonical chain | kept |
| `BlockVersionKey { block_id }`, `LiveVersionKey { v }` | the fork only | **deleted** (`VersionRegistry::release_writes`) |
| Base index (`NullifierKey`), `GcKey` | finalized blocks only | untouched; fork blocks never reach them |
| Overlay `index`, `BlockRecord`s | the fork's states | freed when the states are dropped from `Ledger` (`rpds` refcounts) |

`Imt::discard_writes(version) -> WriteBatch` reads nothing. The caller finds
the version with `VersionRegistry::lookup(block_id)`. A stale block that was
never committed has no version, so there is nothing to discard.

### One LIB advance: `Ledger::apply_pruning(new_lib, &PrunedBlocks)`

1. **Finalize**: the finalize writes of the `new_lib` state, and
   `release_writes` for the newly finalized blocks (the `immutable_blocks`
   and `new_lib` itself).
2. **Discard**: `discard_writes` and `release_writes` for every
   `pruned.stale_blocks()`, in each note set.
3. **Write steps 1 and 2 in one `write_batch`.** Their writes touch disjoint
   keys.
4. **Only after the write**, in memory: replace the `new_lib` state with the
   finalized one, and remove the states of `stale_blocks` and
   `immutable_blocks`.
5. **Collect garbage**: compute `floor_len` per note set from the LIB and the
   epoch snapshots held by the remaining states, then apply
   `collect_garbage(floor_len)` as a second batch.

If a batch fails, neither the memory nor the KV changes, and the error is
returned to chain-service.

### Edge cases

| Case | Behaviour |
|---|---|
| Reorg (the tip moves to another fork above the LIB) | Nothing to do: every fork stays readable and nothing is deleted until the LIB decides |
| Pruned forks sharing blocks | The engine returns each block once (`prune_fork` stops at already removed blocks), so each version is discarded once |
| Block prepared but never committed | No version and no writes, so discarding is a no-op |
| A clone of a discarded state still held (API call in flight, wallet, leader) | Reads fail with `ImtError::MissingNode(NodeKey)`, distinct from `Storage` |
| Crash between the engine's pruning and the batch | On restart, the fork's `LiveVersionKey` is not reused by the replay, and the startup GC discards it |
| Crash between the finalize batch and the GC batch | The `G` keys stay; the next `collect_garbage` handles them |
| Epoch snapshot below the LIB | Readable through the length filter and the GC floor; after a restart through `from_snapshot` |
| Engine and ledger disagree (a surviving descendant of a discarded block) | Impossible given `prune_fork`; could be checked in debug builds through `unfinalized_versions` |

### Tests (`merkle/imt/src/store/tests.rs`)

A simulator builds blocks on random forks over one `MemKv`. Each block carries
the oracle of its state, the current `IndexedMerkleTree`. The simulator writes
exactly as the ledger would: commit; finalize plus discard in one batch, then
GC. It can pin states below the LIB, like epoch snapshots, and restart, losing
forks, with pins coming back through `from_snapshot`. After every step:

- every live and pinned state matches its oracle on `len`, `root`,
  `contains`, `low_leaf`, `leaf` and `path`;
- every node reachable from a live or pinned root is stored;
- every unreachable stored node has a stale record;
- every stale record belongs to a live block or to one waiting for GC;
- the `G` keys are above the floor;
- the registered versions are exactly those of the live blocks above the LIB;
- the base index holds exactly the LIB's nullifiers;
- once every pin is released and the garbage collected, the stored nodes are
  exactly the reachable ones.

Plus targeted tests:

- siblings sharing a low leaf;
- a fork at the LIB (kept) and one just below it (pruned);
- empty blocks;
- a discarded clone fails with `MissingNode`;
- rebase;
- a pinned snapshot reading below the LIB;
- a snapshot surviving a restart;
- restart with lost forks;
- errors (zero, duplicate in the base, the overlay and the pending block,
  `UncommittedBlock`, `NotInitialized`, `Full`);
- ordering near the modulus;
- reseal;
- storage errors propagate;
- codec round trips and key ordering.

Mutation checks were run by hand, each reverted afterwards. Disabling the
deletion of replaced nodes, the GC floor, or the length filter of `contains`
each makes the suite fail.

---

## 4. `NoteSet` and `Ledger` integration

- `note_set.rs`:
  - `NullifierImt = store::Imt<Prefixed<DynKvStore>, ZkHasher, 40>`, with
    `DynKvStore = Arc<dyn KvStore<Error = StoreError>>` from `lb-kv-store`
    (wrap a backend with `ErasedKv::into_dyn`), so that `LedgerState` does
    not become generic. Each note set gets its own `Prefixed` namespace; the
    `VersionRegistry` uses one more, shared.
  - `NoteSet::empty()` takes the store and the note set index.
  - `seal()` runs at the end of each block, where `flush` runs today.
  - `NoteSetError` derives `Copy`. `ImtError` is `Clone` + `Eq`, not `Copy`,
    because it can carry a storage error, so `NoteSetError` and the errors
    built on it lose `Copy`.
- `Ledger`:
  - New fields: the store and the `VersionRegistry`.
  - `prepare_update` rebases the parent state's trees. Its result keeps the
    pending writes in memory.
  - `commit_update` becomes fallible (`Result`). It allocates a version,
    commits both note sets, writes one batch, and only then inserts the state.
  - `prune_state_at` is replaced by `apply_pruning(new_lib, &PrunedBlocks)`
    (section 3). It computes the GC floor per note set from the remaining
    states' LIB and epoch snapshots (`epoch_state.note_sets`).
  - Genesis (`from_notes`, `from_genesis_tx`) calls `Imt::initialize`, which
    writes the sentinel at version 0 together with `BaseMeta`.
- Errors: `ImtError::Storage` and `ImtError::MissingNode(NodeKey)`,
  propagated through `NoteSetError`, `TransferError`, `SdpError`, the channel
  errors and `LedgerError`.
- Serde:
  - The trees of a saved `LedgerState` are canonical states at or below the
    base: the LIB's own sets, and its epoch snapshot. They are serialized as
    `Snapshot { len, root }`.
  - Decoding is followed by an attach step (`Imt::from_snapshot`, plus
    `check_base` for the LIB's own sets), which needs the store.
  - `PartialEq` compares `(len, root.hash)`.
- Callers to review (they will now receive `Result`):
  - `ledger/tests/gas_micro.rs` and `ledger/benches/gas/micro.rs`
  - `zk/proofs/{poq,pol}/src/wallet_inputs.rs`
  - the wallet service
  - `c-bindings/src/api/pow.rs`
  - chain-leader's block proposal, which reads states that are prepared but
    not committed. That works, because reads go to `pending` first.

---

## 5. Node wiring

- A RocksDB `KvStore` on a dedicated column family, through a synchronous
  `Arc<DB>` handle:
  - `get_prev` uses `seek_for_prev`
  - `DeletePrefix` uses `delete_range_cf`
  - bloom filters on `H` and `N`
- Chain-service:
  - It passes the whole `PrunedBlocks` to `Ledger::apply_pruning` instead of
    `prune_ledger_states(pruned_blocks.all())`. This covers `lib.rs:474`,
    `lib.rs:662`, and the pbp, following and ibd phases.
  - Finalize, discard and the removal of the states happen in that order, with
    a single batch.
- Restart:
  1. Restore the LIB state and run `attach_store`.
  2. Replay the blocks from the LIB to the tip. `commit_update` **reuses** the
     version found in `BlockVersionKey`: the recomputed nodes are
     byte-identical, so the writes are idempotent.
  3. Discard every `LiveVersionKey` that the replay did not reuse (forks lost
     at the restart, or a crash before a pruning batch).
- Crash consistency:
  - Commit, finalize and pruning are each one atomic batch.
  - The remaining gap is between `BaseMeta` and the separately saved
    `CryptarchiaConsensusState`. Either save the consensus state in the same
    RocksDB batch as the finalize (preferred), or detect a mismatch at startup
    and rebuild.
- Migration: existing data directories hold a full-leaf `NullifierImt` in the
  saved state. Either a one-shot import (bulk build at version 0, plus the base
  index), or a resync on devnets.

---

## 6. Benches and gas

- `ledger/benches/gas/micro.rs` (`execute_spending`) against `MemKv` and
  RocksDB, at 2^20 to 2^26 leaves, with a cold and a warm `NodeCache`.
- Per block, measure: the size of the commit batch, the time of the finalize
  and of the pruning, and the disk taken by the fork window, including the
  writes wasted on pruned forks.
- Recalibrate the gas constants if they move.
- Memory test: flat at O(B · b) over N blocks.
- Disk test: about 2N nodes plus the fork window as the LIB advances.

---

## Complexity

Symbols:

- N: finalized nullifiers
- H = 40: tree height
- b: inserts per block
- k: blocks since the LIB
- B: blocks kept across all forks
- r: one KV read (a cache hit for the top levels)
- h: one Poseidon2 compression

| | Cost |
|---|---|
| **Memory** | O(B · b): nullifier overlays and `BlockRecord`s only (about 50 MB at B = 2200, b = 100). O(1) per state otherwise |
| **Disk** | O(N), about 250 B raw per nullifier, plus O(B · b · (H − log b)) nodes and stale records for the fork window |
| `clone` | O(1) |
| `contains` / low-leaf lookup | O(log(k·b) + r); a state below the base pays one more `get_prev` per newer nullifier between its predecessor and the bound |
| `leaf(i)` / `path(i)` | O(H · r) |
| `insert` | O(log(k·b) + H · r) |
| `seal` | O(b · (H − log b)) reads of the dirty committed nodes, plus O(b · H · h) hashing |
| `commit` | one batch of about b·(H − log b) nodes, plus as many stale records; no reads |
| `finalize` (per finalized block) | b index puts and 1 `G` key; no reads |
| `collect_garbage` (per collected block) | 1 scan of its stale records, then O(s) deletes (s ≈ its node writes) |
| **Disk retained for snapshots** | the nodes the canonical chain replaced since the oldest retained snapshot (about an epoch of blocks) |
| Discard (per stale block) | O(1) prefix deletes, plus 2 point deletes; space is reclaimed by compaction |
| Rebase (per finalized block) | O(b · log(k·b)) |
| Serializing the IMT part of `LedgerState` | O(1) |
| Restart | O(k) block replays, with idempotent writes |

For comparison, today:

- memory is O(N) and grows forever;
- serializing the saved state is O(N);
- deserializing it rebuilds the whole tree in O(N · h).

---

## Comparison with Zcash and Aztec

Drawn from memory of zcashd, Zebra and Aztec's barretenberg `world_state`. The
storage internals of Aztec in particular must be checked against the source
before we rely on them.

| | **This plan** | **Zcash: zcashd** | **Zcash: Zebra** | **Aztec** |
|---|---|---|---|---|
| Nullifier structure | IMT, height 40 | plain set in a DB, no tree | plain sets in RocksDB column families | IMT, height 40 |
| Non-membership proof | in the circuit, through the low leaf | none (nodes check the set) | none | in the circuit, through the low leaf |
| Low-leaf rewrite | yes | n/a | n/a | yes |
| Leaf | `H(DST, nullifier, next_nullifier, next_index)` | n/a | n/a | `H(value, next_index, next_value)` |
| Insertion | one at a time, hashing deferred per block | set insert | set insert | batched per tx as a subtree |
| Fork model | many forks validated and readable up to the LIB | a single active chain; a reorg disconnects and reconnects blocks | non-finalized forks in memory (≤ 100 blocks), finalized state in RocksDB | essentially linear; reorgs unwind blocks; short-lived forks for simulation and block building |
| Fork isolation | CoW nodes keyed by `(version, level, index)`, plus an overlay for the index | undo data per block, layered coins cache | one in-memory `Chain` per fork | an in-memory fork cache over committed state stored per block |
| Backend | `KvStore` trait (RocksDB, `MemKv`) | LevelDB | RocksDB | LMDB |
| Node keys | position + version | n/a | n/a | content-addressed (hash), with refcounts |
| Fork memory | O(B · b), the index overlay only | coins cache | O(non-finalized blocks × their sets and trees) | uncommitted fork caches |
| GC of replaced nodes | stale records per version, deleted once the retention floor passes the finalized block | n/a | n/a | refcounts, freed when historical blocks are removed |
| Dropping a pruned fork | `DeletePrefix` on its versions, in the finalize batch | n/a (undo at disconnect) | drop the `Chain` | unwind the block or discard the fork cache |
| Reads in the past | states above the LIB, plus canonical snapshots down to the retention floor (epoch snapshots) | the tip only | each fork's tip, plus finalized | historical blocks by number (retention is configurable) |
| Finality | LIB (security parameter k) | none formal | 100 blocks | proven and finalized on L1 |

Takeaways:

1. Zcash avoids the problem: its nullifiers are not in a tree. Zebra's fork
   model matches the in-memory diff alternative (see below). That validates
   the approach, but Zebra only keeps sets in memory, not tree nodes.
2. For the tree, Aztec is the closest match: the same structure and height,
   backed by a disk store. Its chain is almost linear, however, so it never
   has to keep many forks readable at once. Hence our versions per block
   rather than an unwind log.
3. **Node keys** are the real point of divergence. Content addressing with
   refcounts deduplicates identical subtrees, but adds refcount writes on
   every node touched, and a crash between refcount updates can leak nodes or
   free live ones. Position + version never mutates a key, drops a fork with
   O(1) prefix deletes, and needs no refcount consistency; the cost is no
   deduplication across forks, which forks seldom need. We keep
   position + version.
4. Aztec-style batched subtree insertion would cut the hashing in `seal`, but
   our root depends on insertion in execution order. That would be a change to
   the **spec**, not to the storage.

---

## Alternatives considered

| Alternative | Why rejected |
|---|---|
| Overwriting keys in place in the KV | Sibling forks collide on the low leaf, on the appended leaf position, on the index and on the nodes |
| A single live view plus undo logs (Bitcoin / zcashd) | `Ledger` must keep many fork states readable at once (validating side forks, wallet and prover queries); every access to another fork would mean switching heads |
| Versioned keys for everything (MVCC) | Reads need a fork-aware "latest version on my ancestry" lookup; GC is hard |
| An in-memory diff per state over the LIB base (layer chain, or one cumulative `rpds` diff with lazy rebase) | Correct and simpler, but memory O(B · b · H), dominated by node hashes (about 0.6–1 GB at B = 2200, b = 100). Still worth it if the real k·b turns out small |
| Removing the low-leaf rewrite (e.g. a sparse Merkle tree keyed by nullifier) | Changes the spec and the circuits (`NULLIFIER_IMT_LEAF_V1`) |

---

## Phases

| Phase | PR | Main risk |
|---|---|---|
| 1 | `kv-store` crate + `MemKv` + conformance suite | low |
| 2 | imt: layout, codecs, walk, insert / seal / commit on a single chain | correctness of the walk and the siblings (oracle tests) |
| 3 | imt: index overlay, finalize, fork pruning, rebase, GC | GC invariants (reachability test) |
| 4 | `NoteSet` / `Ledger` integration, `apply_pruning` | `Result` propagation; `commit_update` becomes fallible |
| 5 | RocksDB + chain-service | stale vs immutable pruning, version reuse at restart, crash consistency, migration |
| 6 | benches + gas | per-insert cost of H reads (mitigated by `NodeCache`) |

Phases 1 to 3 stay within `merkle/`, change no behaviour and can be merged
first.

---

## Open decisions

1. Scope of the next iteration (phase 4 alone, or 4–6).
2. ~~`gc_lag`~~: replaced by the retention floor. The ledger has to compute
   it from the LIB state and the epoch snapshots of the remaining states, one
   floor per note set. Also check what `dd0afacc8` keeps just below the LIB:
   a canonical state there is readable as long as it is counted in the floor.
3. `discard_lag`: 0, or a margin for readers still holding clones of pruned
   states.
4. Node keys: position + version (proposed) or content-addressed with
   refcounts (Aztec).
5. RocksDB: share the storage service's DB or a dedicated one; whether the
   finalize and the consensus state are written in the same atomic batch.
6. Migration: import or resync.
7. Size and eviction policy of `NodeCache`.
8. Whether the commitment MMR, the eligible tree and the recent-root snapshots
   later move to the same trait, since they share the growth problem.
