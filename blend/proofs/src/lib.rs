use lb_blend_crypto::{ZkHash, ZkHasher};
pub use lb_poq::CorePathAndSelectors;
use lb_poseidon2::Digest;

pub mod quota;
pub mod selection;

trait ZkHashExt {
    fn hash(&self) -> ZkHash;
}

impl<T> ZkHashExt for T
where
    T: AsRef<[ZkHash]>,
{
    fn hash(&self) -> ZkHash {
        // let mut hasher = ZkHasher::new();
        // hasher.update(self.as_ref());
        // hasher.finalize();
        <ZkHasher as Digest>::digest(self.as_ref())
    }
}

trait ZkCompressExt {
    fn compress(&self) -> ZkHash;
}

impl ZkCompressExt for [ZkHash; 2] {
    fn compress(&self) -> ZkHash {
        <ZkHasher as Digest>::compress(self)
    }
}

impl ZkCompressExt for &[ZkHash; 2] {
    fn compress(&self) -> ZkHash {
        <ZkHasher as Digest>::compress(self)
    }
}
