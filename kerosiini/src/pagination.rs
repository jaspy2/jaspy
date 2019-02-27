pub struct PaginatedQueryResult<T> {
    pub records: Vec<T>,
    pub page: i64,
    pub page_size: i64,
    pub total: i64
}
