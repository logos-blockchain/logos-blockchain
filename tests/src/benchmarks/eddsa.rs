#[cfg(test)]
mod tests {
    #[cfg(target_arch = "x86_64")]
    use std::hint::black_box;

    #[cfg(target_arch = "x86_64")]
    use ed25519_dalek::{Signature, Signer as _, SigningKey};
    #[cfg(target_arch = "x86_64")]
    use rand::rngs::OsRng;

    #[cfg(target_arch = "x86_64")]
    #[expect(
        clippy::undocumented_unsafe_blocks,
        reason = "This test is just to measure cpu and should be run manually"
    )]
    #[test]
    fn bench_verify_cycles() {
        let iters = 1000u64;

        let mut csprng = OsRng;
        let signing_key: SigningKey = SigningKey::generate(&mut csprng);
        let message: &[u8] = b"Logos blockchain will rule the world someday.";
        let signature: Signature = signing_key.sign(message);

        let t0 = unsafe { core::arch::x86_64::_rdtsc() };
        for _ in 0..iters {
            let _ = black_box(signing_key.verify(message, &signature));
        }
        let t1 = unsafe { core::arch::x86_64::_rdtsc() };

        let cycles_diff = t1 - t0;
        let cycles_per_run = (t1 - t0) / iters;

        println!("================= eddsa-verify =================");
        println!("  - iterations        : {iters:>20}");
        println!("  - cycles total      : {cycles_diff:>20}");
        println!("  - cycles per run    : {cycles_per_run:>20}");
        println!("================================================\n");
    }
}
