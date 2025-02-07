use super::{
    headers::{
        header_slice_status_watch::HeaderSliceStatusWatch,
        header_slices::{HeaderSlice, HeaderSliceStatus, HeaderSlices},
    },
    verification::{header_slice_verifier::HeaderSliceVerifier, parallel::map_parallel},
};
use crate::sentry_connector::chain_config::ChainConfig;
use parking_lot::RwLock;
use std::{ops::DerefMut, sync::Arc, time::SystemTime};
use tracing::*;

/// Verifies the block structure and sequence rules in each slice and sets VerifiedInternally status.
pub struct VerifySlicesStage {
    header_slices: Arc<HeaderSlices>,
    chain_config: ChainConfig,
    verifier: Arc<Box<dyn HeaderSliceVerifier>>,
    pending_watch: HeaderSliceStatusWatch,
}

impl VerifySlicesStage {
    pub fn new(
        header_slices: Arc<HeaderSlices>,
        chain_config: ChainConfig,
        verifier: Arc<Box<dyn HeaderSliceVerifier>>,
    ) -> Self {
        Self {
            header_slices: header_slices.clone(),
            chain_config,
            verifier,
            pending_watch: HeaderSliceStatusWatch::new(
                HeaderSliceStatus::Downloaded,
                header_slices,
                "VerifySlicesStage",
            ),
        }
    }

    pub async fn execute(&mut self) -> anyhow::Result<()> {
        self.pending_watch.wait().await?;

        debug!(
            "VerifySlicesStage: verifying {} slices",
            self.pending_watch.pending_count()
        );
        self.verify_pending().await;
        Ok(())
    }

    async fn verify_pending(&self) {
        loop {
            let slices_batch = self
                .header_slices
                .find_batch_by_status(HeaderSliceStatus::Downloaded, num_cpus::get());
            if slices_batch.is_empty() {
                break;
            }

            let slices_verified = self.verify_slices_parallel(&slices_batch).await;

            for (i, slice_lock) in slices_batch.iter().enumerate() {
                let mut slice = slice_lock.write();
                let is_verified = slices_verified[i];

                if is_verified {
                    self.header_slices
                        .set_slice_status(slice.deref_mut(), HeaderSliceStatus::VerifiedInternally);
                } else {
                    self.header_slices
                        .set_slice_status(slice.deref_mut(), HeaderSliceStatus::Invalid);
                }
            }
        }
    }

    async fn verify_slices_parallel(&self, slices: &[Arc<RwLock<HeaderSlice>>]) -> Vec<bool> {
        map_parallel(Vec::from(slices), |slice_lock| -> bool {
            let mut slice = slice_lock.write();
            Self::prepare_slice_hashes(&mut slice);
            self.verify_slice(&slice)
        })
        .await
    }

    fn prepare_slice_hashes(slice: &mut HeaderSlice) {
        if let Some(headers) = slice.headers.as_mut() {
            for header in headers {
                header.hash_prepare();
            }
        }
    }

    fn now_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn verify_slice(&self, slice: &HeaderSlice) -> bool {
        let Some(headers) = slice.headers.as_ref() else {
            return false;
        };

        self.verifier.verify_slice(
            headers,
            slice.start_block_num,
            Self::now_timestamp(),
            self.chain_config.chain_spec(),
        )
    }

    pub fn can_proceed_check(&self) -> impl Fn() -> bool {
        let header_slices = self.header_slices.clone();
        move || -> bool { header_slices.contains_status(HeaderSliceStatus::Downloaded) }
    }
}

#[async_trait::async_trait]
impl super::stage::Stage for VerifySlicesStage {
    async fn execute(&mut self) -> anyhow::Result<()> {
        Self::execute(self).await
    }
    fn can_proceed_check(&self) -> Box<dyn Fn() -> bool + Send> {
        Box::new(Self::can_proceed_check(self))
    }
}
