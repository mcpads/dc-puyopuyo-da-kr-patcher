use super::*;
use sh4::Instruction;

#[test]
fn dreamcast_profile_round_trips_nop_at_an_aligned_code_location() {
    let location = 0x8c01_0000;
    let encoded = <DreamcastSh4Profile as TypedIsa>::encode_one(&Instruction::Nop, &location)
        .expect("SH-4 NOP must encode at an aligned executable address");
    assert_eq!(encoded.as_slice(), &[0x09, 0x00]);

    let decoded = <DreamcastSh4Profile as TypedIsa>::decode_one(encoded.as_slice(), &location)
        .expect("typed SH-4 bytes must decode under the same profile");
    assert_eq!(decoded.instruction(), &Instruction::Nop);
    assert_eq!(decoded.source().as_slice(), encoded.as_slice());
    assert_eq!(profile_id(), "sh4-pr0-sz0-le");
}

#[test]
fn dreamcast_profile_rejects_unaligned_code_locations() {
    let error = <DreamcastSh4Profile as TypedIsa>::encode_one(&Instruction::Nop, &0x8c01_0001)
        .expect_err("SH-4 instructions cannot start at an odd address");
    assert_eq!(
        error,
        sh4::EncodeError::UnalignedCodeLocation {
            location: 0x8c01_0001
        }
    );
}
