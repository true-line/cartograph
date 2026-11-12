use std::env;

pub fn load(name: &str) -> (Option<String>, Option<String>) {
    (env::var("DATABASE_URL").ok(), env::var(name).ok())
}

mod fake {
    fn var(key: &str) -> String {
        key.to_string()
    }

    pub fn lookalike() -> String {
        var("SHADOWED")
    }
}
