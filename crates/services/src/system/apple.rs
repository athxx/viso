//! Pieces the macOS and iOS dialogs share.

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_uniform_type_identifiers::UTType;

use crate::files::{FileFilter, PickedFile};
use crate::reply::{ServiceError, ServiceResult};

/// The content types `filters` allow; empty when they allow everything.
pub(super) fn content_types(filters: &[FileFilter]) -> Retained<NSArray<UTType>> {
    let types: Vec<Retained<UTType>> = filters
        .iter()
        .flat_map(|f| &f.extensions)
        .filter_map(|e| UTType::typeWithFilenameExtension(&NSString::from_str(e)))
        .collect();
    NSArray::from_retained_slice(&types)
}

/// Read every picked file in full.
pub(super) fn read_urls(urls: &NSArray<NSURL>) -> ServiceResult<Vec<PickedFile>> {
    urls.iter()
        .map(|url| {
            let path = url
                .to_file_path()
                .ok_or_else(|| ServiceError::Failed("a picked URL is not a file".into()))?;
            super::read_picked(path)
        })
        .collect()
}
