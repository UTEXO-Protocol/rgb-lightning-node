use std::time::Duration;

pub(crate) const RGS_SYNC_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub(crate) const RGS_SNAPSHOT_MAX_SIZE: usize = 15 * 1024 * 1024;
pub(crate) const RGS_SYNC_TIMEOUT_SECS: u64 = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgs_constants_have_expected_values() {
        assert_eq!(RGS_SYNC_INTERVAL.as_secs(), 60 * 60);
        assert_eq!(RGS_SNAPSHOT_MAX_SIZE, 15 * 1024 * 1024);
        assert_eq!(RGS_SYNC_TIMEOUT_SECS, 5);
    }
}
