#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("the catalog is not valid JSON for this version of Stockroom: {0}")]
    Malformed(String),
    #[error("the catalog breaks a rule: {0}")]
    Invalid(String),
    #[error("the catalog uses format {0}, which needs a newer version of Stockroom")]
    UnsupportedSchema(u32),
    #[error("the catalog is older (serial {got}) than one already seen (serial {seen})")]
    Older { got: u64, seen: u64 },
}
