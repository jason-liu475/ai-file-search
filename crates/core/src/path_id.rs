#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PathId {
    normalized: String,
}

impl PathId {
    #[must_use]
    pub fn from_user_path(path: &str) -> Self {
        let normalized = path
            .replace('\\', "/")
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect::<Vec<_>>()
            .join("/");

        Self { normalized }
    }

    #[must_use]
    pub fn as_normalized(&self) -> &str {
        &self.normalized
    }
}
