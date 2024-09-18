pub fn generate_uid() -> String {
    uuid::Uuid::new_v4().to_string()
}
