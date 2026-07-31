use std::borrow::Cow;

use lb_utils::bounded::BoundedVec;

use crate::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError, sealed,
};

#[derive(Debug, Clone, Copy)]
enum NOfBytes {
    One,
    Two,
    Four,
    Eight,
}

const fn length_prefix_width<const MAX_LENGTH: usize>() -> NOfBytes {
    if MAX_LENGTH <= u8::MAX as usize {
        NOfBytes::One
    } else if MAX_LENGTH <= u16::MAX as usize {
        NOfBytes::Two
    } else if MAX_LENGTH <= u32::MAX as usize {
        NOfBytes::Four
    } else {
        NOfBytes::Eight
    }
}

/// Byte-width of the length prefix for a `MAX_LENGTH`-bounded collection.
const fn length_prefix_len<const MAX_LENGTH: usize>() -> usize {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => 1,
        NOfBytes::Two => 2,
        NOfBytes::Four => 4,
        NOfBytes::Eight => 8,
    }
}

fn encode_length_prefix_into<const MAX_LENGTH: usize>(actual_length: usize, out: &mut Vec<u8>) {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => u8::try_from(actual_length)
            .expect("Actual length should be smaller than u8 MAX_LENGTH")
            .encode_into(out),
        NOfBytes::Two => u16::try_from(actual_length)
            .expect("Actual length should be smaller than u16 MAX_LENGTH")
            .encode_into(out),
        NOfBytes::Four => u32::try_from(actual_length)
            .expect("Actual length should be smaller than u32 MAX_LENGTH")
            .encode_into(out),
        NOfBytes::Eight => u64::try_from(actual_length)
            .expect("Actual length should be smaller than u64 MAX_LENGTH")
            .encode_into(out),
    }
}

fn decode_length_prefix<const MAX_LENGTH: usize>(
    input: &[u8],
) -> Result<(&[u8], usize), DecodeError> {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => u8::decode(input, &()).map(|(rest, len)| (rest, usize::from(len))),
        NOfBytes::Two => u16::decode(input, &()).map(|(rest, len)| (rest, usize::from(len))),
        NOfBytes::Four => u32::decode(input, &()).map(|(rest, len)| {
            (
                rest,
                len.try_into().expect("usize should be able to hold u32"),
            )
        }),
        NOfBytes::Eight => u64::decode(input, &()).map(|(rest, len)| {
            (
                rest,
                len.try_into().expect("usize should be able to hold u64"),
            )
        }),
    }
}

/// Upper bound, in elements, on how much a decoder may pre-allocate from a
/// declared length before decoding anything. Beyond this the `Vec` grows
/// amortised, so the memory a peer can make us commit stays proportional to the
/// bytes it actually sent.
const PREALLOC_LIMIT: usize = 1024;

/// Largest `MAX` for which an element type that decodes without consuming input
/// is still supported.
///
/// Such an element makes the decode loop independent of the input, so `MAX` is
/// the only thing bounding it — repeating it is harmless exactly when `MAX` is
/// small enough that a hostile length prefix buys nothing. Above this, the
/// combination is refused rather than iterated.
const ZERO_LENGTH_ELEMENT_MAX: usize = 1024;

impl<T, const MIN: usize, const MAX: usize> BinaryEncode for BoundedVec<T, MIN, MAX>
where
    T: BinaryEncode,
{
    fn encoded_length(&self) -> usize {
        length_prefix_len::<MAX>()
            .checked_add(self.iter().map(BinaryEncode::encoded_length).sum::<usize>())
            .expect("Encoded length overflow")
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        encode_length_prefix_into::<MAX>(self.len(), out);
        for item in self.iter() {
            item.encode_into(out);
        }
    }
}

impl<T, const MIN: usize, const MAX: usize> BinaryDecode for BoundedVec<T, MIN, MAX>
where
    T: BinaryDecode,
{
    type Context = T::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (mut rest, len) = decode_length_prefix::<MAX>(input)?;

        // Check the length before decoding, so an oversized prefix never causes
        // us to decode a too-large payload.
        if len < MIN || len > MAX {
            return Err(DecodeError::length_out_of_bounds::<Self>(len, MIN, MAX));
        }

        // Pre-allocate up to a max limit to avoid memory amplification attacks.
        let mut items = Vec::with_capacity(len.min(PREALLOC_LIMIT));

        for _ in 0..len {
            let (next, item) = T::decode(rest, context)?;

            // If we decode more than `ZERO_LENGTH_ELEMENT_MAX` correctly without consuming
            // the input, bail out to avoid hanging for too long.
            // TODO: This logic can be made a compile-time check once generics become more
            // powerful and we get const generics expressions. For now, we just check it at
            // runtime.
            if next.len() == rest.len() && MAX > ZERO_LENGTH_ELEMENT_MAX {
                return Err(DecodeError::zero_length_element::<T>(
                    MAX,
                    ZERO_LENGTH_ELEMENT_MAX,
                ));
            }

            rest = next;
            items.push(item);
        }
        Ok((rest, Self::new_unchecked(items)))
    }
}

impl<T, const MIN: usize, const MAX: usize> sealed::Sealed for BoundedVec<T, MIN, MAX> where
    T: CodecExamples
{
}

// The fixture is derived from the element's fixture, giving *every*
// `BoundedVec` monomorphization compile-time fixture existence for free. It
// reuses `encode_length_prefix_into`, so it is circular w.r.t. the length
// prefix — prefix drift is covered by the hand-pinned `#[test]`s below.
//
// `MIN` may be 0 (`UpperBoundedVec`), so we force at least one element;
// otherwise the fixture would be empty and never touch `T`'s codec. The floor
// is capped by `MAX`, since `[0, 0]` is a legal bound and a one-element fixture
// there would violate the type's own invariant (and fail to round-trip).
impl<T, const MIN: usize, const MAX: usize> CodecExamples for BoundedVec<T, MIN, MAX>
where
    T: CodecExamples,
{
    fn fixtures() -> CodecFixtures<Self> {
        let count = MIN.max(1).min(MAX);

        let mut values = Vec::with_capacity(count);
        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(count, &mut bytes);
        for _ in 0..count {
            let item = T::fixtures()
                .into_iter()
                .next()
                .expect("`CodecExamples::fixtures` is non-empty");
            bytes.extend_from_slice(item.bytes.as_ref());
            values.push(item.value);
        }

        [CodecFixture {
            value: Self::new_unchecked(values),
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedVec;

    use crate::{
        BinaryDecodeExt as _, BinaryEncode as _, CodecExamples as _, DecodeError,
        assert_codec_fixtures,
    };

    /// Bound used across the tests: between 2 and 4 elements.
    const MIN: usize = 2;
    const MAX: usize = 4;

    type Bounded = BoundedVec<u8, MIN, MAX>;

    /// Builds a `BoundedVec` for encoding tests, bypassing the length checks so
    /// the codec itself remains the thing under test.
    fn bounded(items: &[u8]) -> Bounded {
        Bounded::new_unchecked(items.to_vec())
    }

    #[test]
    fn encode_prepends_a_single_byte_length_prefix() {
        assert_eq!(bounded(&[1, 2, 3]).encode_to_vec(), vec![3, 1, 2, 3]);
    }

    #[test]
    fn encode_at_the_min_and_max_lengths() {
        assert_eq!(bounded(&[1, 2]).encode_to_vec(), vec![2, 1, 2]);
        assert_eq!(bounded(&[1, 2, 3, 4]).encode_to_vec(), vec![4, 1, 2, 3, 4]);
    }

    #[test]
    fn decode_reads_a_well_formed_payload() {
        let (rest, bv) = Bounded::decode(&[3, 1, 2, 3]).unwrap();
        assert!(rest.is_empty());
        assert_eq!(bv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        let (rest, bv) = Bounded::decode(&[2, 1, 2, 99, 100]).unwrap();
        assert_eq!(rest, &[99, 100]);
        assert_eq!(bv.as_slice(), &[1, 2]);
    }

    #[test]
    fn decode_at_the_min_and_max_lengths() {
        let (_, at_min) = Bounded::decode(&[2, 1, 2]).unwrap();
        assert_eq!(at_min.as_slice(), &[1, 2]);

        let (_, at_max) = Bounded::decode(&[4, 1, 2, 3, 4]).unwrap();
        assert_eq!(at_max.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn decode_rejects_a_length_below_min() {
        let err = Bounded::decode(&[1, 7]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 1, .. }));
    }

    #[test]
    fn decode_rejects_a_zero_length() {
        let err = Bounded::decode(&[0]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 0, .. }));
    }

    #[test]
    fn decode_rejects_a_length_above_max() {
        let err = Bounded::decode(&[5, 1, 2, 3, 4, 5]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 5, .. }));
    }

    #[test]
    fn decode_rejects_an_oversized_length_even_without_a_payload() {
        let err = Bounded::decode(&[5]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 5, .. }));
    }

    #[test]
    fn decode_fails_on_an_empty_input() {
        let err = Bounded::decode(&[]).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn decode_fails_when_the_length_prefix_is_truncated() {
        // A 2-byte prefix is expected (via a `u16::MAX` maximum), but only 1
        // byte is available.
        type WideCodec = BoundedVec<u8, MIN, { u16::MAX as usize }>;
        let err = WideCodec::decode(&[0]).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn decode_fails_when_the_payload_is_truncated() {
        let err = Bounded::decode(&[3, 1]).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let original = bounded(&[10, 20, 30, 40]);
        let bytes = original.encode_to_vec();
        let (rest, decoded) = Bounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn roundtrips_with_a_multi_byte_item_type() {
        type U16Codec = BoundedVec<u16, MIN, MAX>;
        let original: U16Codec = U16Codec::new_unchecked(vec![0x0102, 0x0304, 0xABCD]);

        let bytes = original.encode_to_vec();
        // 1-byte length prefix (3) (MAX == 4) followed by three little-endian `u16`s.
        assert_eq!(bytes, vec![3, 0x02, 0x01, 0x04, 0x03, 0xCD, 0xAB]);

        let (rest, decoded) = U16Codec::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn two_byte_length_prefix() {
        type TwoByteBounded = BoundedVec<u8, 1, { u16::MAX as usize }>;
        let original: TwoByteBounded = [10].into();
        let bytes = original.encode_to_vec();
        assert_eq!(bytes, vec![1, 0, 10]); // 2-byte length prefix (1) then a single `u8` (10)
        let (rest, decoded) = TwoByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn four_byte_length_prefix() {
        type FourByteBounded = BoundedVec<u8, 1, { u32::MAX as usize }>;
        let original: FourByteBounded = FourByteBounded::new_unchecked(vec![10]);
        let bytes = original.encode_to_vec();
        assert_eq!(bytes, vec![1, 0, 0, 0, 10]); // 4-byte length prefix (1) then a single `u8` (10)
        let (rest, decoded) = FourByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    /// Paired with a bound too large to iterate, a zero-length element type
    /// turns the decode loop into a pure instruction count driven by the
    /// prefix, which no amount of input truncation bounds. Before this
    /// combination was refused, this exact call ran for minutes without
    /// finishing: eight bytes buy `u64::MAX` iterations.
    ///
    /// If it regresses, this test hangs rather than fails.
    #[test]
    fn a_zero_length_element_type_cannot_drive_an_unbounded_loop() {
        type ZeroLength = BoundedVec<[u8; 0], 0, { u64::MAX as usize }>;

        let err = ZeroLength::decode(&[0xFF; 8]).unwrap_err();

        assert!(matches!(err, DecodeError::ZeroLengthElement { .. }));
    }

    /// Under a small bound the same element type is supported: `MAX` caps the
    /// loop at a harmless number of no-op iterations, so there is nothing to
    /// refuse. The count is all such an encoding carries, and it survives the
    /// round trip.
    #[test]
    fn a_zero_length_element_type_roundtrips_under_a_small_bound() {
        type ZeroLength = BoundedVec<[u8; 0], 0, 4>;

        let original = ZeroLength::new_unchecked(vec![[]; 3]);
        let bytes = original.encode_to_vec();
        assert_eq!(bytes, vec![3]); // the length prefix is the whole encoding

        let (rest, decoded) = ZeroLength::decode(&bytes).unwrap();

        assert!(rest.is_empty());
        assert_eq!(decoded, original);
        assert_eq!(decoded.len(), 3);
    }

    /// Support is decided by `MAX` alone, so it is a property of the type
    /// rather than of the message: a type either always works or always fails,
    /// and which one is visible from its declaration.
    #[test]
    fn zero_length_element_support_turns_on_the_bound_not_the_message() {
        type AtTheLimit = BoundedVec<[u8; 0], 0, { super::ZERO_LENGTH_ELEMENT_MAX }>;
        type PastTheLimit = BoundedVec<[u8; 0], 0, { super::ZERO_LENGTH_ELEMENT_MAX + 1 }>;

        // A 2-byte prefix for both bounds, declaring a single element.
        let input = 1u16.to_le_bytes();

        let (_, decoded) = AtTheLimit::decode(&input).unwrap();
        assert_eq!(decoded.len(), 1);

        let err = PastTheLimit::decode(&input).unwrap_err();
        assert!(matches!(err, DecodeError::ZeroLengthElement { .. }));
    }

    /// The check must fire on the elements, not on the type: decoding *no*
    /// elements never loops, so an empty collection stays decodable.
    #[test]
    fn a_zero_length_element_type_still_decodes_when_empty() {
        type ZeroLength = BoundedVec<[u8; 0], 0, 4>;

        let (rest, decoded) = ZeroLength::decode(&[0]).unwrap();

        assert!(rest.is_empty());
        assert!(decoded.is_empty());
    }

    /// The cap must not cost correctness: payloads longer than the
    /// pre-allocation limit still decode, they just grow the `Vec` as bytes are
    /// consumed.
    #[test]
    fn a_payload_longer_than_the_prealloc_limit_still_decodes() {
        const LEN: usize = super::PREALLOC_LIMIT + 1;
        type Wide = BoundedVec<u8, 1, { u16::MAX as usize }>;

        let mut input = u16::try_from(LEN).unwrap().to_le_bytes().to_vec();
        input.extend(std::iter::repeat_n(0xAB, LEN));

        let (rest, decoded) = Wide::decode(&input).unwrap();

        assert!(rest.is_empty());
        assert_eq!(decoded.len(), LEN);
        assert!(decoded.iter().all(|&b| b == 0xAB));
    }

    /// `[0, 0]` is a legal bound: the only inhabitant is the empty vector, so
    /// the fixture must *not* apply the usual "at least one element" floor.
    #[test]
    fn zero_max_fixture_is_empty_and_roundtrips() {
        type Empty = BoundedVec<u8, 0, 0>;

        let fixtures = Empty::fixtures();
        let fixture = fixtures.first().unwrap();
        assert!(fixture.value.is_empty());
        assert_eq!(fixture.bytes.as_ref(), &[0]); // just a 1-byte length prefix (0)

        assert_codec_fixtures::<Empty>();
    }

    #[test]
    fn zero_max_rejects_any_non_empty_length() {
        type Empty = BoundedVec<u8, 0, 0>;

        let err = Empty::decode(&[1, 7]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 1, .. }));
    }

    #[test]
    fn eight_byte_length_prefix() {
        type EightByteBounded = BoundedVec<u8, 1, { u64::MAX as usize }>;
        let original: EightByteBounded = EightByteBounded::new_unchecked(vec![10]);
        let bytes = original.encode_to_vec();
        assert_eq!(bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 10]); // 8-byte prefix (1) then a single `u8`
        let (rest, decoded) = EightByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }
}

#[cfg(test)]
mod allocation_tests {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    use lb_utils::bounded::BoundedVec;

    use crate::{BinaryDecodeExt as _, DecodeError};

    /// Runs `f` and reports how many bytes it allocated on this thread.
    fn bytes_allocated_by<F, R>(f: F) -> (R, usize)
    where
        F: FnOnce() -> R,
    {
        let before = ALLOCATED_BYTES.get();
        let result = f();
        (result, ALLOCATED_BYTES.get() - before)
    }

    /// Forwards to the system allocator, tallying every byte handed out. Growth
    /// is counted too, so a `Vec` that reallocates as it fills is not free.
    ///
    /// Installed for the whole test binary — every test in this crate allocates
    /// through it — but it only adds a counter bump on top of `System`.
    struct CountingAllocator;

    // SAFETY: every method forwards its arguments unchanged to `System`, which
    // upholds the `GlobalAlloc` contract. The only added work is a thread-local
    // counter bump, which allocates nothing and so cannot re-enter the
    // allocator.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record_allocation(layout.size());
            // SAFETY: `layout` is forwarded untouched from our caller.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record_allocation(layout.size());
            // SAFETY: `layout` is forwarded untouched from our caller.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` was handed out by `System` under `layout`, since
            // every allocating method here delegates to it.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            record_allocation(new_size.saturating_sub(layout.size()));
            // SAFETY: as `dealloc`; `new_size` is forwarded untouched.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    // The tally is thread-local, not a global counter: the harness runs the
    // tests in this binary in parallel, each on its own thread, so a global one
    // would attribute their allocations to whoever happens to be measuring.
    //
    // `const`-initialised so reading it neither allocates nor registers a
    // destructor — either would re-enter the allocator below.
    thread_local! {
        static ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
    }

    fn record_allocation(bytes: usize) {
        // `try_with` because TLS is gone while a thread is being torn down, and
        // an allocation at that point is not part of any measurement anyway.
        let _ = ALLOCATED_BYTES.try_with(|counter| counter.set(counter.get() + bytes));
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    /// A declared length of `u16::MAX` backed by a single item must cost us
    /// roughly one item, not `u16::MAX` of them. Without the cap the decoder
    /// reserves the full declared length up front: ~512 KiB from a 10-byte
    /// input, a >50,000x amplification an attacker gets for free on every
    /// message.
    #[test]
    fn a_large_declared_length_does_not_preallocate_from_the_wire() {
        type Wide = BoundedVec<u64, 1, { u16::MAX as usize }>;

        const DECLARED: u16 = u16::MAX;
        /// What reserving the declared length outright would cost.
        const UNCAPPED_COST: usize = DECLARED as usize * size_of::<u64>();

        // Declares `u16::MAX` items but carries only one.
        let mut input = DECLARED.to_le_bytes().to_vec();
        input.extend_from_slice(&7u64.to_le_bytes());

        let (err, allocated) = bytes_allocated_by(|| Wide::decode(&input).unwrap_err());

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
        assert!(
            allocated < UNCAPPED_COST / 8,
            "decoding a {} byte input allocated {allocated} bytes; \
             reserving the declared length would cost {UNCAPPED_COST}",
            input.len(),
        );
    }

    /// A declared length must never be pre-allocated on trust: with a
    /// `usize::MAX` bound nothing rejects it up front, so reserving it outright
    /// aborts the process on a 8-byte input instead of returning an error.
    #[test]
    fn huge_declared_length_does_not_preallocate_from_the_wire() {
        type Wide = BoundedVec<u8, 1, { u64::MAX as usize }>;

        let err = Wide::decode(&u64::MAX.to_le_bytes()).unwrap_err();

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }
}
