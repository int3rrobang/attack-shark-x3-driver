use attack_shark_x3::protocol::checksum::sum16;

// Evidence: wrapping 16-bit sum used by X3 reports 0x05 and 0x08 and DPI 0x04
// See docs/protocols/04-dpi.md, 05-preferences.md, 08-button-mapping.md and
// crates/attack-shark-x3/src/protocol/checksum.rs.

#[test]
fn checksum_empty_is_zero() {
    assert_eq!(sum16(&[]), 0x0000);
}

#[test]
fn checksum_single_bytes_boundaries() {
    assert_eq!(sum16(&[0x00]), 0x0000);
    assert_eq!(sum16(&[0xff]), 0x00ff);
    assert_eq!(sum16(&[0x01]), 0x0001);
}

#[test]
fn checksum_simple_carry_without_truncating_to_eight_bits() {
    // 0xff + 0x02 = 0x0101 – low-byte truncation would give 0x01
    assert_eq!(sum16(&[0xff, 0x02]), 0x0101);
    assert_eq!(sum16(&[0xff, 0xff]), 0x01fe);
    assert_eq!(sum16(&[0xff, 0x01]), 0x0100);
}

#[test]
fn checksum_low_byte_overflow_to_high_byte() {
    // 0x00ff + 0x01 = 0x0100
    assert_eq!(sum16(&[0xff, 0x01]), 0x0100);
    // 0x00ff + 0xff = 0x01fe
    assert_eq!(sum16(&[0xff, 0xff]), 0x01fe);
    // 0xff + 0xff + 0x02 = 0x0200
    assert_eq!(sum16(&[0xff, 0xff, 0x02]), 0x0200);
}

#[test]
fn checksum_max_boundary_ffff() {
    // 257 * 0xff = 0xffff – maximum before wrap
    assert_eq!(sum16(&[0xff; 257]), 0xffff);
    // single max value plus complement
    assert_eq!(sum16(&[0xff, 0xff, 0xff]), 0x02fd);
}

#[test]
fn checksum_wrapping_at_sixteen_bits() {
    // 258 * 0xff = 0x00fe – wraps past 0xffff
    assert_eq!(sum16(&[0xff; 258]), 0x00fe);
    // 512 * 0xff = 0xfe02? Let's compute: 512*255=130560=0x1fe00 -> 0xfe00 masked
    assert_eq!(sum16(&[0xff; 512]), 0xfe00);
    // 0xffff + 1 wraps to 0
    let mut max_plus_one = vec![0xff; 257];
    max_plus_one.push(0x01);
    assert_eq!(sum16(&max_plus_one), 0x0000);
    // 0xffff + 0xff wraps to 0x00fe
    let mut max_plus_ff = vec![0xff; 257];
    max_plus_ff.push(0xff);
    assert_eq!(sum16(&max_plus_ff), 0x00fe);
}

#[test]
fn checksum_carry_across_high_byte() {
    // 0xff00 + 0x0100 = 0x0000 wrap
    assert_eq!(sum16(&[0xff; 256]), 0xff00);
    assert_eq!(
        sum16(
            &[0xff; 256]
                .iter()
                .copied()
                .chain(std::iter::once(0x01))
                .collect::<Vec<_>>()
        ),
        0xff01
    );
    // verify wrapping addition is correct for known sums
    assert_eq!(sum16(&[0x01; 0x100]), 0x0100);
    assert_eq!(sum16(&[0x01; 0x10000]), 0x0000);
}

#[test]
fn checksum_deterministic_and_order_independent_sum() {
    let a = [0x10, 0x20, 0x30, 0x40];
    let b = [0x40, 0x30, 0x20, 0x10];
    assert_eq!(sum16(&a), sum16(&b));
    assert_eq!(sum16(&a), 0x00a0);
}

#[test]
fn checksum_all_zeros_stays_zero() {
    assert_eq!(sum16(&[0x00; 0]), 0x0000);
    assert_eq!(sum16(&[0x00; 1]), 0x0000);
    assert_eq!(sum16(&[0x00; 100]), 0x0000);
    assert_eq!(sum16(&[0x00; 1000]), 0x0000);
}

#[test]
fn checksum_known_dpi_slice_mimic() {
    // Mimic bytes[3..50] of a DPI report – simple known sum
    // Use the captured DPI report payload from fixtures/protocol/dpi.json first entry bytes 3..50 sum should match packet 50..52
    // Here we verify the arithmetic itself is correctly wrapping
    let bytes = [0x01u8; 47]; // 47 * 1 = 47
    assert_eq!(sum16(&bytes), 47);
    let bytes_ff = [0xffu8; 47]; // 47*255=11985=0x2ed1
    assert_eq!(sum16(&bytes_ff), 0x2ed1);
}
