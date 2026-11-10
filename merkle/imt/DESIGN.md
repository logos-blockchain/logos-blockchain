# KV-backed, fork-safe nullifier IMT — design plan (v8)

Status:

- Phases 1–3 are implemented: `merkle/kv-store` and `merkle/imt/src/store/`.
- Phase 4 is implemented: `NoteSet` keeps its nullifier tree in the store, and
  `Ledger` commits, finalizes, prunes and collects it
  (`ledger/src/nullifier_storage.rs`).
- Phase 5 is split in two. 5a is implemented: `RocksKv`, a `KvStore` on a
  `RocksDB` database of its own (`lb-kv-store`, feature `rocksdb`). 5b is
  implemented: the node keeps the trees in that database, saved states
  encode them as snapshots, and a restart resumes from an anchor in the
  store (§5b).
- Phase 6 is not started.

Changes since v7: §5b as implemented. The recovery state no longer holds the
LIB's ledger state, which the anchor replaces; §4's "Serde, until phase 5" is
superseded.

Changes since v6, the decisions of phase 5 (see §5):

- The store is a dedicated `RocksDB` database, not a column family of the
  storage service's. Its type is a parameter of the chain service, given in
  the overwatch services declaration of the node.
- The gap between the saved consensus state and the store is closed by an
  anchor written in the pruning batch, not by a shared batch.
- Existing data directories are not migrated: they resync.

Changes since v5, all found while implementing phase 4 (see §4):

- The retention floor is option A: the shortest tree any kept state holds,
  computed per note set over the three note sets of every kept state. See
  §3, "Garbage collection and retained snapshots".
- `discard_lag` is 0: a pruned fork is deleted in the pruning batch.
- Note sets are created at runtime (channel sets), possibly by two forks at
  the same index. A new tree starts with no write (`Imt::empty`) and its
  store gets a base at its first finalize.
- Until phase 5, a ledger state is still serialized with every leaf, in the
  format of the in-memory tree, and a decoded one is imported in a store of
  its own. `Snapshot` and the attach step move to phase 5, with RocksDB.
- `NoteSets::new()` keeps a store in memory of its own. The ledger takes its
  store from the note sets of its root state.

Changes since v4:

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
- `Prefixed<S>` namespaces each note set's keys under the namespace of the
  set (§3).
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
and `Imt::import`, which write a whole tree as a finalized base; `Imt::empty`
starts a tree without writing. The caller applies the batch, together with
its own writes, before it uses the returned state, so a failed write leaves
memory and store as they were.

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

`floor_len` is the retention floor of the note set, computed by the ledger
(`retention_floors` in `ledger/src/nullifier_storage.rs`), option A below.

#### The retention floor

Who reads an old tree:

- Leader proofs are the only reader of nullifier paths. `pol_wallet_inputs`
  reads the low leaf and path of a nullifier in two trees: the current set of
  the tip state, and its `epoch_state.note_sets`, frozen at the stake
  distribution snapshot.
- Each `LedgerState` holds three versions of each note set: its own
  `note_sets`, `epoch_state.note_sets` and `next_epoch_state.note_sets`. The
  next epoch snapshot becomes the epoch snapshot at the epoch transition.
- Everything else reads roots only, which need no old node. The wallet states
  kept below the LIB (`dd0afacc8`) hold commitment trees and aged paths, no
  nullifier tree.

**Option A, implemented: the shortest tree held by a kept state.** For each
note set, `floor_len` is the smallest `len` among the three note sets of
every state the ledger keeps after the pruning: the LIB, its descendants, and
their epoch snapshots. A set a state does not hold, a channel set created
after its snapshot was taken, does not bound the floor of that set.

- It is exact: it holds whatever the epoch rules are, without assuming that
  the snapshots of the descendants are at least as long as those of the LIB.
- It costs a pass over the lengths of the kept states at each LIB advance:
  O(states × sets), no read.
- It lives in one place, `Ledger::apply_pruning`, right after the finalize
  batch.
- In practice the floor is the length of the oldest epoch snapshot of the
  LIB, up to about an epoch below it. The disk held for it is the nodes the
  canonical chain replaced over that window, about 40 per insert.

Options not taken:

| | Floor | Why not |
|---|---|---|
| B | `lib_state.epoch_state.note_sets[i].len()` | O(1), but relies on the snapshots only moving forward along a chain, checked by a `debug_assert` at best. A takes the same minimum without the assumption, at a negligible cost. |
| C | Pins: snapshots handed to readers carry a guard, the floor is the smallest live one | Protects readers outside the ledger too, such as RPC, but needs shared state and `Drop` hooks, and the pins to be restored at restart. It can be added on top of A if such readers appear. |
| D | Keep everything inserted in the last *K* epochs | A guess: too small breaks leader proofs, too large wastes disk, and it needs tuning per era. |

A state of a pruned fork read after the pruning, such as a chain-leader
proposal on a tip pruned meanwhile, fails with `MissingNode`. The proposal
fails as it would on a pruned fork today.

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
   `pruned.stale_blocks()`, in each note set the block recorded at its commit
   (`W ‖ version`).
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

## 4. `NoteSet` and `Ledger` integration (implemented)

### Note sets (`core/src/mantle/note_set.rs`)

- `NullifierImt = store::Imt<NoteSetStore, ZkHasher, 40>`, with
  `NoteSetStore = Prefixed<DynKvStore>` and
  `DynKvStore = Arc<dyn KvStore<Error = StoreError>>`, so that `LedgerState`
  does not become generic.
- `NoteSets` holds the store. Each set lives in its own namespace, named by
  the set rather than by its eligible leaf: `note_set_namespace(index)` is
  `L` for the ledger set, `S` for the SDP set and `C ‖ channel_id` (32 bytes)
  for a channel set. No namespace is a prefix of another. The
  `VersionRegistry` lives in `V`, and the sets each block wrote to in `W`
  (see the ledger below), both shared by every set.
  - `NoteSets::new()` keeps a `MemKv` of its own: tests, benches and the node
    until phase 5. `NoteSets::with_store(store)` takes the store.
  - `NoteSet::empty()` keeps a `MemKv` of its own;
    `NoteSet::empty_in(store)` takes the namespace of a set.
- Dynamic sets. Channels add sets at runtime (`with_channel`), and two forks
  can each create the set of the same channel. A channel's eligible leaf
  depends on the order of creation on its fork, its namespace does not. A new set starts with `Imt::empty`:
  the sentinel in a pending block, sealed, nothing read or written. The block
  creating it writes it under its own version like any other block, and the
  namespace gets a `BaseMeta` at its first finalize. Until then, a store
  without base reads as empty, and `rebase` leaves the tree as is.
- `seal()` runs at the end of each block, where `flush` ran. The eligible leaf
  takes the root of the sealed tree (`Imt::sealed_root`), which reads nothing.
- Fallible now: `nullifier_root`, `contains_nullifier`, `end_block`,
  `end_block_with_updates`. `NoteSetError` loses `Copy` (`ImtError` can carry
  a storage error), and so does `LeaderWitnessError`, which gains a
  `NoteSet` variant.
- Lifecycle, each returning the writes of every set as one batch of the
  store (`Prefixed::namespace_batch`): `NoteSets::commit(version)`,
  `finalize(block_id)`, `rebase()`, and the static `discard_writes(store,
  written, version)` and `collect_garbage(store, index, floor_len)`.
  `commit` also returns the sets it wrote to (`CommittedSets::written`).
- `PartialEq`: a tree compares by `len` and root (`Imt` hashes a pending
  block not sealed yet). `NoteSets` ignores its store; `NoteSet` and
  `NoteSets` are no longer `Eq`, nor is `EpochState`.

### Ledger (`ledger/src/lib.rs`, `ledger/src/nullifier_storage.rs`)

- `NullifierStorage` holds the store, the `VersionRegistry`, and under `W`
  the sets each committed block wrote to, keyed by its version. The ledger
  takes the store from the note sets of its root state.
- `Ledger::new(id, state, eras)` becomes fallible. The root state becomes the
  finalized base of the store, committed first if it has a pending block, as
  a genesis state does. The epoch snapshots of a genesis state are its own
  note sets, committed with them.
- `commit_update(update)` becomes fallible. It allocates the version of the
  block, commits the three note sets of the state, writes one batch with the
  registry entry and the record of the sets it wrote to, and only then keeps
  the state. The epoch snapshots of a
  block are those of its parent, committed already.
- `prune_state_at` is replaced by `apply_pruning(lib, &PrunedBlocks)`, a
  no-op when nothing is pruned:
  1. One batch: the LIB's own note sets are finalized; everything the stale
     blocks wrote is deleted, in the sets each recorded, so a channel only a
     stale fork created is cleaned up too; the versions of the pruned blocks
     and of the LIB are released with their records.
  2. The pruned states are removed and the kept ones rebased.
  3. One batch: the garbage collection of each set of the LIB, with the
     retention floor of §3 (option A).
- A state kept by the ledger cannot change: its trees are in the store. Tests
  that changed a state after committing it now change it before
  (`update_ledger_amended` in the cryptarchia tests).

### Chain service

- `Cryptarchia::from_genesis` becomes fallible (`Ledger::new`); `from_lib`
  is replaced by `from_restored` in phase 5b.
- A failed `commit_update` is an `Error::Ledger`. Only a storage failure fails
  it, and it leaves the consensus ahead of the ledger: the block is known but
  not its state.
- `prune_ledger_states(&PrunedBlocks)` calls `apply_pruning` with the LIB of
  the consensus. A failure is logged: the states stay in place, and the next
  pruning finalizes the newer LIB.

### Serde, until phase 5 (superseded by §5b)

- A `NoteSet` is still encoded with every leaf, in the format of the
  in-memory tree, so saved states keep decoding.
- Decoding checks the leaves with the in-memory tree, then imports them in a
  store kept in memory, one per `NoteSets` (`Imt::import`, at
  `Version::GENESIS`). Encoding walks every committed node once
  (`Imt::leaves`).
- Phase 5 replaces this with `Snapshot { len, root }` and an attach step to
  the RocksDB store, plus a one-shot import of the existing data directories.

### Tests

- `merkle/imt/src/store/tests.rs`: `Imt::empty` on two forks of one store,
  `import` and `leaves` round trips, equality.
- `ledger/src/nullifier_storage.rs`: the ledger and the consensus engine
  together, against the in-memory tree as oracle: a chain while the LIB
  advances, a pruned fork leaving no key in the store, an epoch snapshot below
  the LIB readable until replaced and then collected, two forks creating the
  same channel set, a restored LIB state, and the retention floors.
- Mutation checks: a floor from the LIB alone fails the snapshot and restart
  tests; skipping the discard of pruned forks fails the fork tests.

---

## 5. Node wiring

### 5a. `RocksKv` (implemented)

`lb_kv_store::RocksKv`, behind the `rocksdb` feature, so that the crates
holding the trees (core, ledger) depend on the trait only.

- It owns a database of its own and uses its default column family.
- `get_prev` iterates between the prefix (lower bound) and the key, clamped
  to the end of the prefix (upper bound, exclusive), from the last key.
- `scan_prefix` iterates between the prefix and its end.
- A `WriteBatch` becomes one `RocksDB` batch: atomic, and in order, since its
  writes take increasing sequence numbers. `DeletePrefix` is a
  `delete_range` to the end of the prefix, so a put after it in the same
  batch survives. A prefix without end (empty, or `0xff` bytes only) deletes
  up to the greatest key it covers, stored or put earlier in the batch.
- `RocksKvOptions`:
  - `sync_writes` (off by default): without it, a crash of the machine may
    lose the last batches, never tear or reorder them. A crash of the
    process loses nothing.
  - `bloom_bits_per_key` (10 by default): a bloom filter on whole keys, for
    `get` on absent keys. Filters on prefixes would need a prefix extractor
    that knows the key layout of the tree; they wait for phase 6.
- Tests: the conformance suite, with and without the bloom filter; reopening
  the database; prefixes without end. The fork simulation of the tree store
  (`test_forks_match_oracle`) also runs on `RocksKv`, and fails when
  `get_prev` ignores its key bound.

### 5b. Chain service and restart (implemented)

- The database:
  - A dedicated `RocksDB` database at `<state folder>/nullifiers`, next to
    the storage service's. The chain service opens it in `init`, so a store
    that cannot open stops the service before it starts, and owns it: the
    ledger reads and writes it synchronously, while the storage service is
    reached through async messages.
  - Its type is a parameter of the chain service,
    `CryptarchiaConsensus<Tx, TimeBackend, NullifierStore, RuntimeServiceId>`,
    given in the overwatch services declaration of the node (`RocksKv` there,
    `MemKv` in tests). `lb_kv_store::OpenKvStore` opens it from
    `KvStoreSettings { path, sync_writes }` (in `CryptarchiaSettings`,
    `nullifier_store`; the node sets `sync_writes: false`, see §5a). The
    settings are concrete: overwatch's `ServiceState` impl leaves no room for
    settings generic over the store.
  - The genesis state is built in memory, then moved to the store
    (`LedgerState::with_nullifier_store`, `NoteSets::move_to`).
- Serde: a saved state encodes its trees as `Snapshot { len, root }` instead
  of every leaf (`EncodedNullifiers`); encoding a tree with pending
  nullifiers is an error. Decoding gives trees over `DetachedKv`, where every
  read fails, until `NoteSets::attach` puts them on the store
  (`Imt::from_snapshot`, plus `check_base` for the LIB's own sets).
- The anchor. The consensus state is saved by the storage service on a timer
  (`state_recording_timer`), while the store's base moves at every pruning.
  After a crash the saved LIB can be older than the base, and the garbage
  collection may have deleted nodes of its trees. So the pruning batch also
  writes an anchor (key `A` at the store's root, `Anchor { lib, state, info }`
  in `ledger/src/lib.rs`): the LIB's id, its parent, length, slot and uncle
  slots (`LibInfo`), and its ledger state, which `Snapshot` makes small. Its
  size per pruning is not measured yet (phase 6). `Ledger::new_anchored`
  writes the first one at genesis, `apply_pruning_anchored` the next ones,
  and `Ledger::restore` reads it back (`Restored { ledger, lib, info }`).
  At restart (`cryptarchia_from_store` in the chain service):
  1. Start from the anchor's LIB, which the store is consistent with. The
     saved state's LIB is usually older; it is newer only after a failed
     pruning, whose blocks the replay applies again.
  2. Replay the stored blocks from that LIB to the saved tip. If the tip does
     not descend from it, fall back to the LIB, as the recovery does today.
     `commit_update` **reuses** the version found in `BlockVersionKey`: the
     recomputed nodes are byte-identical, so the writes are idempotent.
  3. Discard every `LiveVersionKey` that the replay did not reuse (forks lost
     at the restart, or a crash before a pruning batch):
     `Ledger::discard_lost_blocks`, each in the sets it recorded under `W`,
     so a lost fork's channel set needs no state to be cleaned up.

  Without an anchor, a node whose saved LIB is the genesis clears the store
  and starts from the genesis; any other node stops with `Error::NoAnchor`,
  asking to delete its chain data and sync.
- The recovery state (`CryptarchiaConsensusState`) no longer holds the LIB's
  ledger state: the anchor holds it, consistent with the store.
- Crash consistency: commit, finalize and pruning are each one atomic batch,
  and the anchor is in the finalize's.
- Migration: none. A saved state in the old format, with every leaf, fails
  to decode; the node stops with a message to delete its data directory and
  resync.
- Tests:
  - `ledger/src/nullifier_storage.rs`: a restored LIB state; a decoded state
    unattached until attached; a restart that replays the blocks and
    discards the lost forks; a restart reopening a `RocksKv` on disk; a store
    without anchor.
  - `core/src/mantle/note_set.rs`: snapshot round trips, attach, and sets
    never committed moved to another store.
  - chain-service: the recovery tests on an anchored `MemKv`.
  - Mutation check: dropping the anchor write in the pruning fails three
    restart tests.

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
| 4 | `NoteSet` / `Ledger` integration, `apply_pruning` (done) | `Result` propagation; `commit_update` becomes fallible |
| 5a | `RocksKv` (done) | ordered reads and range deletes match `MemKv` (conformance, fork simulation) |
| 5b | chain-service on `RocksKv`, `Snapshot` serde, anchor, restart (done) | version reuse at restart, crash consistency |
| 6 | benches + gas | per-insert cost of H reads (mitigated by `NodeCache`) |

Phases 1 to 3 stay within `merkle/`, change no behaviour and can be merged
first.

---

## Open decisions

1. ~~Scope of the next iteration~~: phase 4 alone, stacked on phases 1–3.
2. ~~Retention floor~~: option A, the shortest tree any kept state holds
   (§3). `dd0afacc8` keeps wallet states below the LIB, which hold no
   nullifier tree.
3. ~~`discard_lag`~~: 0. A pruned fork is deleted in the pruning batch.
4. Node keys: position + version (proposed) or content-addressed with
   refcounts (Aztec).
5. ~~RocksDB~~: a dedicated database, its type a parameter of the chain
   service; the anchor instead of a shared batch (§5b).
6. ~~Migration~~: resync.
7. Size and eviction policy of `NodeCache`.
8. Whether the commitment MMR, the eligible tree and the recent-root snapshots
   later move to the same trait, since they share the growth problem.
