/// Computes the wrapping 16-bit sum used by X3 configuration reports.
#[must_use]
pub fn sum16(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0_u16, |sum, byte| sum.wrapping_add(u16::from(*byte)))
}

#[cfg(test)]
mod tests {
    use super::sum16;

    #[test]
    fn sums_bytes_without_truncating_to_eight_bits() {
        assert_eq!(sum16(&[0xff, 0x02]), 0x0101);
    }

    #[test]
    fn wraps_at_sixteen_bits() {
        assert_eq!(sum16(&[0xff; 258]), 0x00fe);
    }
}
